//! Private subprocess lifecycle. Protocol parsing stays in each backend.
use crate::BackendCommandOutput;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use smartzip_core::{Result, SmartZipError};
use std::{
    future::Future,
    io,
    path::Path,
    process::{ExitStatus, Stdio},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    task::{JoinError, JoinHandle},
};
use tokio_util::sync::CancellationToken;

pub(crate) const MAX_OUTPUT: usize = 16 * 1024 * 1024;
pub(crate) type Pipe = Box<dyn AsyncRead + Unpin + Send>;
type Capture = (Vec<u8>, bool);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Wait for process, then collect pipes; reject truncated output.
    Ordinary,
    /// Same lifecycle, but the backend continues interpreting bounded progress records.
    Streaming,
    /// Preserve diagnostic locale, pre-start cancellation and concurrent read/wait errors.
    Diagnostic,
}

pub(crate) async fn bounded_read(mut stream: impl AsyncRead + Unpin) -> io::Result<Capture> {
    let mut retained = Vec::new();
    let mut buffer = [0; 16 * 1024];
    let mut truncated = false;
    loop {
        let size = stream.read(&mut buffer).await?;
        if size == 0 {
            break;
        }
        let keep = size.min(MAX_OUTPUT.saturating_sub(retained.len()));
        retained.extend_from_slice(&buffer[..keep]);
        truncated |= keep < size;
    }
    Ok((retained, truncated))
}

#[derive(Debug)]
enum Failure {
    Spawn(io::Error),
    Wait(io::Error),
    Read(io::Error),
    ReaderJoin(JoinError, Option<i32>),
    Cancelled {
        _kill_error: Option<io::Error>,
        wait_error: Option<io::Error>,
    },
    OutputLimit,
    MissingPipe(&'static str),
}
impl Failure {
    fn into_error(self, executable: &Path, id: &str, mode: Mode) -> SmartZipError {
        match self {
            Self::Spawn(source) if source.kind() == io::ErrorKind::NotFound => {
                SmartZipError::BackendUnavailable { backend: id.into() }
            }
            Self::Spawn(source) | Self::Wait(source) => {
                SmartZipError::io(Some(executable.into()), source)
            }
            Self::Read(source) => SmartZipError::io(
                (mode == Mode::Diagnostic).then(|| executable.into()),
                source,
            ),
            Self::ReaderJoin(source, exit_code) if mode == Mode::Streaming => {
                SmartZipError::BackendFailed {
                    backend: "7zz".into(),
                    exit_code,
                    stderr: source.to_string(),
                }
            }
            Self::ReaderJoin(source, _) => SmartZipError::io(
                (mode == Mode::Diagnostic).then(|| executable.into()),
                io::Error::other(source),
            ),
            Self::Cancelled {
                wait_error: Some(source),
                ..
            } if mode != Mode::Diagnostic => SmartZipError::io(Some(executable.into()), source),
            Self::Cancelled { .. } => SmartZipError::Cancelled,
            Self::OutputLimit => SmartZipError::ResourceLimit {
                detail: "backend output exceeded 16 MiB; incomplete output was rejected".into(),
            },
            Self::MissingPipe(pipe) if mode == Mode::Diagnostic => {
                SmartZipError::BackendProtocolError {
                    backend: id.into(),
                    detail: format!("missing test {pipe} pipe"),
                }
            }
            Self::MissingPipe(pipe) => SmartZipError::io(
                Some(executable.into()),
                io::Error::other(format!("7z child {pipe} pipe was unavailable")),
            ),
        }
    }
}

fn spawn(executable: &Path, args: &[String], mode: Mode) -> io::Result<Box<dyn ChildWrapper>> {
    #[cfg(target_os = "linux")]
    let parent_pid = unsafe { libc::getpid() };
    let mut command = CommandWrap::with_new(executable, |command| {
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if mode == Mode::Diagnostic {
            command.env("LC_ALL", "C");
            #[cfg(unix)]
            command.process_group(0);
        }
        #[cfg(target_os = "linux")]
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent_pid {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "SmartZip parent exited before backend startup",
                    ));
                }
                Ok(())
            });
        }
    });
    // Preserve the existing diagnostic parent-wait semantics (and Windows
    // parent-only cancellation) instead of silently changing them during cleanup.
    if mode != Mode::Diagnostic {
        #[cfg(unix)]
        command.wrap(process_wrap::tokio::ProcessGroup::leader());
        #[cfg(windows)]
        command.wrap(process_wrap::tokio::JobObject);
    }
    command.wrap(KillOnDrop);
    command.spawn()
}

struct Reader(Option<JoinHandle<io::Result<Capture>>>);
impl Drop for Reader {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}
impl Reader {
    fn spawn(future: impl Future<Output = io::Result<Capture>> + Send + 'static) -> Self {
        Self(Some(tokio::spawn(future)))
    }
    async fn collect(&mut self, status: Option<i32>) -> std::result::Result<Capture, Failure> {
        let result = self.0.as_mut().expect("reader collected once").await;
        self.0.take();
        result
            .map_err(|error| Failure::ReaderJoin(error, status))?
            .map_err(Failure::Read)
    }
    async fn stop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

async fn cancel_child(
    child: &mut dyn ChildWrapper,
    mode: Mode,
    initial_pid: Option<u32>,
) -> Failure {
    let kill_error = if mode == Mode::Diagnostic {
        #[cfg(unix)]
        if let Some(pid) = initial_pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        Box::into_pin(child.kill()).await.err()
    } else {
        child.start_kill().err()
    };
    // Always attempt wait, including after a failed kill.
    let wait_error = child.wait().await.err();
    Failure::Cancelled {
        _kill_error: kill_error,
        wait_error,
    }
}

async fn run_child(
    mut child: Box<dyn ChildWrapper>,
    mut stdout: Reader,
    mut stderr: Reader,
    token: &CancellationToken,
    mode: Mode,
) -> std::result::Result<(BackendCommandOutput, bool), Failure> {
    // wait() can reap a parent while descendants still hold its pipes.
    // Preserve the original group identity across that await.
    let initial_pid = child.id();
    let completed = async {
        if mode == Mode::Diagnostic {
            let (status, out, err) = tokio::try_join!(
                async { child.wait().await.map_err(Failure::Wait) },
                stdout.collect(None),
                stderr.collect(None)
            )?;
            Ok::<_, Failure>((status, Some((out, err))))
        } else {
            Ok((child.wait().await.map_err(Failure::Wait)?, None))
        }
    };
    let (status, captures): (ExitStatus, _) = tokio::select! {
        result = completed => result?,
        _ = token.cancelled() => {
            let error = cancel_child(child.as_mut(), mode, initial_pid).await;
            stdout.stop().await;
            stderr.stop().await;
            return Err(error);
        }
    };
    let ((stdout, cut_out), (stderr, cut_err)) = if let Some(captures) = captures {
        captures
    } else {
        let out = stdout.collect(status.code()).await?;
        if mode == Mode::Ordinary && out.1 {
            return Err(Failure::OutputLimit);
        }
        let err = stderr.collect(status.code()).await?;
        if mode == Mode::Ordinary && err.1 {
            return Err(Failure::OutputLimit);
        }
        (out, err)
    };
    Ok((
        BackendCommandOutput {
            status: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        },
        cut_out || cut_err,
    ))
}

pub(crate) async fn run<F, Fut>(
    executable: &Path,
    id: &str,
    args: &[String],
    token: &CancellationToken,
    mode: Mode,
    read: F,
) -> Result<(BackendCommandOutput, bool)>
where
    F: Fn(bool, Pipe) -> Fut,
    Fut: Future<Output = io::Result<Capture>> + Send + 'static,
{
    if mode == Mode::Diagnostic && token.is_cancelled() {
        return Err(SmartZipError::Cancelled);
    }
    let mut child = spawn(executable, args, mode)
        .map_err(|e| Failure::Spawn(e).into_error(executable, id, mode))?;
    let stdout = child.stdout().take().map(|p| Box::new(p) as Pipe);
    let stderr = child.stderr().take().map(|p| Box::new(p) as Pipe);
    let pipe = |stream: Option<Pipe>, name| {
        stream
            .or_else(|| (mode == Mode::Ordinary).then(|| Box::new(tokio::io::empty()) as Pipe))
            .ok_or_else(|| Failure::MissingPipe(name).into_error(executable, id, mode))
    };
    let stdout = pipe(stdout, "stdout")?;
    let stderr = pipe(stderr, "stderr")?;
    run_child(
        child,
        Reader::spawn(read(true, stdout)),
        Reader::spawn(read(false, stderr)),
        token,
        mode,
    )
    .await
    .map_err(|error| error.into_error(executable, id, mode))
}

pub(crate) async fn run_bounded(
    executable: &Path,
    id: &str,
    args: &[String],
    token: &CancellationToken,
    mode: Mode,
) -> Result<(BackendCommandOutput, bool)> {
    run(executable, id, args, token, mode, |_, pipe| {
        bounded_read(pipe)
    })
    .await
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        pin::Pin,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };

    #[derive(Debug)]
    struct FaultChild {
        inner: Box<dyn ChildWrapper>,
        fail_kill: bool,
        fail_wait: bool,
        calls: Arc<Mutex<Vec<&'static str>>>,
    }
    impl ChildWrapper for FaultChild {
        fn inner(&self) -> &dyn ChildWrapper {
            self.inner.as_ref()
        }
        fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
            self.inner.as_mut()
        }
        fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
            self.inner
        }
        fn start_kill(&mut self) -> io::Result<()> {
            self.calls.lock().unwrap().push("kill");
            // Perform the real cleanup, then inject the reported failure.
            self.inner.start_kill()?;
            if self.fail_kill {
                Err(io::Error::other("injected kill failure"))
            } else {
                Ok(())
            }
        }
        fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
            Box::pin(async {
                let status = self.inner.wait().await?;
                self.calls.lock().unwrap().push("wait");
                if self.fail_wait {
                    Err(io::Error::other("injected wait failure"))
                } else {
                    Ok(status)
                }
            })
        }
    }
    struct ReaderDrop(Arc<AtomicUsize>);
    impl Drop for ReaderDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn blocked_reader(dropped: &Arc<AtomicUsize>) -> Reader {
        let guard = ReaderDrop(dropped.clone());
        Reader::spawn(async move {
            let _guard = guard;
            std::future::pending::<io::Result<Capture>>().await
        })
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn parent_death_helper() {
        let Some(pid_path) = std::env::var_os("SMARTZIP_PDEATH_PID") else {
            return;
        };
        let output_path = std::env::var("SMARTZIP_PDEATH_OUTPUT").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _runtime = runtime.enter();
        let child = spawn(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                format!("while :; do printf x >> '{output_path}'; sleep 0.01; done"),
            ],
            Mode::Ordinary,
        )
        .unwrap();
        std::fs::write(pid_path, child.id().unwrap().to_string()).unwrap();
        std::mem::forget(child);
        unsafe { libc::_exit(99) }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parent_exit_kills_backend_before_recovery_can_start() {
        let root = tempfile::tempdir().unwrap();
        let pid_path = root.path().join("backend.pid");
        let output_path = root.path().join("backend-output");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("process::tests::parent_death_helper")
            .arg("--ignored")
            .env("SMARTZIP_PDEATH_PID", &pid_path)
            .env("SMARTZIP_PDEATH_OUTPUT", &output_path)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(99));
        let pid: i32 = std::fs::read_to_string(&pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while unsafe { libc::kill(pid, 0) } == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "backend survived its parent"
        );
    }

    #[tokio::test]
    async fn kill_and_wait_faults_still_reap_child_and_join_readers() {
        for mode in [Mode::Ordinary, Mode::Streaming, Mode::Diagnostic] {
            for (fail_kill, fail_wait) in [(true, false), (false, true), (true, true)] {
                let calls = Arc::new(Mutex::new(Vec::new()));
                let child = spawn(
                    Path::new("/bin/sh"),
                    &["-c".into(), "exec sleep 30".into()],
                    mode,
                )
                .unwrap();
                let child = Box::new(FaultChild {
                    inner: child,
                    fail_kill,
                    fail_wait,
                    calls: calls.clone(),
                });
                let dropped = Arc::new(AtomicUsize::new(0));
                let token = CancellationToken::new();
                token.cancel();
                let error = tokio::time::timeout(
                    Duration::from_secs(5),
                    run_child(
                        child,
                        blocked_reader(&dropped),
                        blocked_reader(&dropped),
                        &token,
                        mode,
                    ),
                )
                .await
                .unwrap()
                .unwrap_err();
                assert_eq!(dropped.load(Ordering::SeqCst), 2);
                let calls = calls.lock().unwrap();
                assert_eq!(calls.first(), Some(&"kill"));
                assert_eq!(calls.last(), Some(&"wait"));
                let error = error.into_error(Path::new("/bin/sh"), "injected", mode);
                if fail_wait && mode != Mode::Diagnostic {
                    assert!(matches!(error, SmartZipError::Io { .. }));
                } else {
                    assert!(matches!(error, SmartZipError::Cancelled));
                }
            }
        }
    }

    #[tokio::test]
    async fn wait_failure_aborts_readers_without_waiting_for_their_eof() {
        let child = spawn(
            Path::new("/bin/sh"),
            &["-c".into(), "exit 0".into()],
            Mode::Ordinary,
        )
        .unwrap();
        let child = Box::new(FaultChild {
            inner: child,
            fail_kill: false,
            fail_wait: true,
            calls: Default::default(),
        });
        let dropped = Arc::new(AtomicUsize::new(0));
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            run_child(
                child,
                blocked_reader(&dropped),
                blocked_reader(&dropped),
                &CancellationToken::new(),
                Mode::Ordinary,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(matches!(error, Failure::Wait(_)));
        tokio::time::timeout(Duration::from_secs(2), async {
            while dropped.load(Ordering::SeqCst) != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
