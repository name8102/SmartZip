//! Bounded, exact single-member reads. No filesystem materialization.
use crate::{
    backend::ArchiveAdapter,
    sevenzz::{parse_entries, validate_extraction_listing, SevenZipBackend},
};
#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{CommandWrap, KillOnDrop};
use smartzip_core::{EncodingMode, Result, SmartZipError};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct MemberReadRequest {
    pub archive: PathBuf,
    pub member: PathBuf,
    pub password: Option<String>,
    pub encoding: EncodingMode,
    pub max_bytes: usize,
}

fn safe_member(member: &Path) -> Result<PathBuf> {
    let invalid = || SmartZipError::UnsafeArchivePath {
        entry: member.display().to_string(),
    };
    let raw = member.to_str().ok_or_else(invalid)?;
    // Prevent list-file/selector syntax and ambiguous normalization, even with -spd.
    if raw.len() > 4096
        || raw.starts_with('@')
        || raw
            .chars()
            .any(|c| c.is_control() || matches!(c, '*' | '?' | '\\'))
    {
        return Err(invalid());
    }
    let path = crate::safety::safe_entry_path(raw.as_bytes()).ok_or_else(invalid)?;
    let mut selector = raw;
    while let Some(rest) = selector.strip_prefix("./") {
        selector = rest;
    }
    if path.as_os_str() != std::ffi::OsStr::new(selector) {
        return Err(invalid());
    }
    // Preserve the exact archive selector after validating its safe form.
    Ok(member.to_path_buf())
}
async fn bounded_read(
    mut stream: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            return Ok(bytes);
        }
        if count > limit.saturating_sub(bytes.len()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "member read limit exceeded",
            ));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}
async fn run_bytes(
    executable: &Path,
    id: &str,
    args: &[std::ffi::OsString],
    limit: usize,
    token: &CancellationToken,
    deadline: tokio::time::Instant,
) -> Result<Vec<u8>> {
    if token.is_cancelled() {
        return Err(SmartZipError::Cancelled);
    }
    let mut command = CommandWrap::with_new(executable, |command| {
        command
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    });
    #[cfg(unix)]
    command.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(JobObject);
    command.wrap(KillOnDrop);
    let mut child = command
        .spawn()
        .map_err(|e| SmartZipError::io(Some(executable.into()), e))?;
    let stdout = child.stdout().take().expect("piped stdout");
    let mut stderr = child.stderr().take().expect("piped stderr");
    // Futures are owned here, not detached tasks. Any failure drops all pipe readers
    // before killing and reaping the complete process group.
    let result = tokio::select! { biased;
        _=token.cancelled()=>Err(SmartZipError::Cancelled),
        _=tokio::time::sleep_until(deadline)=>Err(SmartZipError::ResourceLimit { detail:"member preview exceeded 30 seconds".into() }),
        result=async {
            let mut sink = tokio::io::sink();
            let (bytes,_,status)=tokio::try_join!(bounded_read(stdout,limit),tokio::io::copy(&mut stderr,&mut sink),child.wait())
                .map_err(|e| if e.kind()==std::io::ErrorKind::FileTooLarge { SmartZipError::ResourceLimit{detail:format!("member preview exceeds {limit} bytes")} } else { SmartZipError::io(Some(executable.into()),e) })?;
            if !status.success() { return Err(SmartZipError::BackendFailed { backend:id.into(),exit_code:status.code(),stderr:"无法读取归档成员；加密文件请输入密码后重试".into() }); }
            Ok(bytes)
        }=>result,
    };
    if result.is_err() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    result
}

pub(crate) async fn read_member(
    adapter: &dyn ArchiveAdapter,
    request: MemberReadRequest,
    token: CancellationToken,
) -> Result<Vec<u8>> {
    if token.is_cancelled() {
        return Err(SmartZipError::Cancelled);
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let member = safe_member(&request.member)?;
    if request.max_bytes == 0 || request.max_bytes > 32 * 1024 * 1024 {
        return Err(SmartZipError::ResourceLimit {
            detail: "member preview limit must be within 1..=32 MiB".into(),
        });
    }
    let volumes = crate::volumes::VolumeSet::collect(&request.archive)
        .map_err(|e| SmartZipError::io(Some(request.archive.clone()), e))?;
    if volumes.family != crate::volumes::VolumeFamily::Single || volumes.members.len() != 1 {
        return Err(SmartZipError::UnsupportedFormat {
            path: request.archive,
            format: Some("分卷归档暂需解压后查看".into()),
        });
    }
    let executable =
        adapter
            .executable_path()
            .ok_or_else(|| SmartZipError::BackendUnavailable {
                backend: adapter.id().into(),
            })?;
    let archive = std::path::absolute(&request.archive)
        .map_err(|e| SmartZipError::io(Some(request.archive.clone()), e))?;
    let mut common: Vec<std::ffi::OsString> = vec!["-sccUTF-8".into()];
    if let Some(password) = &request.password {
        common.push(format!("-p{password}").into());
    }
    if let Some(encoding) = SevenZipBackend::encoding_arg(&request.encoding) {
        common.push(encoding.into());
    }
    let mut args: Vec<std::ffi::OsString> = vec!["l".into(), "-slt".into()];
    args.extend(common.clone());
    args.push("--".into());
    args.push(archive.as_os_str().into());
    let listing = run_bytes(
        executable,
        adapter.id(),
        &args,
        4 * 1024 * 1024,
        &token,
        deadline,
    )
    .await?;
    let listing = match std::str::from_utf8(&listing) {
        Ok(text) => std::borrow::Cow::Borrowed(text),
        Err(_) if matches!(request.encoding, EncodingMode::Override(_)) => {
            String::from_utf8_lossy(&listing)
        }
        Err(_) => {
            return Err(SmartZipError::BackendProtocolError {
                backend: adapter.id().into(),
                detail: "invalid UTF-8 member listing".into(),
            })
        }
    };
    let listing = listing.as_ref();
    validate_extraction_listing(listing)?;
    // Preview returns one regular member's bytes; extracting links is a separate operation.
    if listing.lines().any(|line| {
        line.strip_prefix("Symbolic Link = ")
            .is_some_and(|value| !value.is_empty())
            || line
                .strip_prefix("Hard Link = ")
                .is_some_and(|value| !value.is_empty())
            || line
                .strip_prefix("Attributes = ")
                .is_some_and(|attributes| {
                    attributes
                        .split_whitespace()
                        .any(|a| a.starts_with('l') && a.len() == 10)
                })
    }) {
        return Err(SmartZipError::UnsafeArchivePath {
            entry: "member preview contains a link".into(),
        });
    }
    let entries = parse_entries(listing);
    if let Some(bytes) =
        crate::decoded_zip::read_member_if_needed(&request, entries.len(), &token, deadline).await?
    {
        return Ok(bytes);
    }
    let matches: Vec<_> = entries
        .iter()
        .filter(|entry| entry.path.as_os_str() == member.as_os_str())
        .collect();
    if matches.len() != 1 {
        return Err(SmartZipError::BackendProtocolError {
            backend: adapter.id().into(),
            detail: "member is absent or ambiguous".into(),
        });
    }
    if matches[0].is_dir {
        return Err(SmartZipError::UnsupportedFormat {
            path: request.archive,
            format: Some("directory member".into()),
        });
    }
    if matches[0]
        .uncompressed_size
        .is_some_and(|size| size > request.max_bytes as u64)
    {
        return Err(SmartZipError::ResourceLimit {
            detail: format!("member exceeds {} bytes", request.max_bytes),
        });
    }
    let mut args: Vec<std::ffi::OsString> = vec![
        "x".into(),
        "-so".into(),
        "-spd".into(),
        "-ssc".into(),
        "-y".into(),
        "-bd".into(),
    ];
    args.extend(common);
    args.push("--".into());
    args.push(archive.into_os_string());
    args.push(member.into_os_string());
    run_bytes(
        executable,
        adapter.id(),
        &args,
        request.max_bytes,
        &token,
        deadline,
    )
    .await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unsafe_and_ambiguous_selectors() {
        for name in [
            "../secret",
            "/etc/passwd",
            "C:\\secret",
            "//host/file",
            "@list",
            "a*",
            "a?",
            "dir/./file",
            "dir\\file",
            "a\nfile",
        ] {
            assert!(safe_member(Path::new(name)).is_err(), "{name}");
        }
        assert_eq!(
            safe_member(Path::new("./dir/file.txt")).unwrap(),
            PathBuf::from("./dir/file.txt")
        );
        assert_eq!(
            safe_member(Path::new("dir/中文 file.txt")).unwrap(),
            PathBuf::from("dir/中文 file.txt")
        );
    }
    #[tokio::test]
    async fn actual_stream_bytes_are_limited() {
        assert_eq!(bounded_read(&b"abc"[..], 3).await.unwrap(), b"abc");
        assert_eq!(
            bounded_read(&b"abcd"[..], 3).await.unwrap_err().kind(),
            std::io::ErrorKind::FileTooLarge
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn process_output_overrun_and_deadline_stop_without_waiting_for_child_sleep() {
        let token = CancellationToken::new();
        let args = vec!["-c".into(), "printf 123456789; sleep 30".into()];
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            run_bytes(
                Path::new("/bin/sh"),
                "test",
                &args,
                4,
                &token,
                tokio::time::Instant::now() + std::time::Duration::from_secs(30),
            ),
        )
        .await
        .unwrap();
        assert!(matches!(result, Err(SmartZipError::ResourceLimit { .. })));
        let args = vec!["-c".into(), "sleep 30".into()];
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            run_bytes(
                Path::new("/bin/sh"),
                "test",
                &args,
                4,
                &token,
                tokio::time::Instant::now() + std::time::Duration::from_millis(50),
            ),
        )
        .await
        .unwrap();
        assert!(matches!(result, Err(SmartZipError::ResourceLimit { .. })));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_stops_a_running_member_process() {
        let token = CancellationToken::new();
        let cancel = token.clone();
        let signal = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel.cancel();
        });
        let args = vec!["-c".into(), "sleep 30".into()];
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            run_bytes(
                Path::new("/bin/sh"),
                "test",
                &args,
                4,
                &token,
                tokio::time::Instant::now() + std::time::Duration::from_secs(30),
            ),
        )
        .await
        .unwrap();
        signal.await.unwrap();
        assert!(matches!(result, Err(SmartZipError::Cancelled)));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn duplicate_members_and_links_never_reach_content_read() {
        use std::os::unix::fs::PermissionsExt;
        for listing in [
            "Path = file.txt\nSize = 1\n\nPath = file.txt\nSize = 1\n",
            "Path = file.txt\nSize = 1\nSymbolic Link = /etc/passwd\n",
        ] {
            let root = tempfile::tempdir().unwrap();
            let executable = root.path().join("backend");
            let archive = root.path().join("input.zip");
            std::fs::write(&archive, b"placeholder").unwrap();
            let script = format!(
                "#!/bin/sh\nif [ \"$1\" = l ]; then\ncat <<'LIST'\n{listing}LIST\nelse\nexit 99\nfi\n"
            );
            std::fs::write(&executable, script).unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            let adapter = SevenZipBackend::new(executable);
            let result = read_member(
                &adapter,
                MemberReadRequest {
                    archive,
                    member: "file.txt".into(),
                    password: None,
                    encoding: EncodingMode::Auto,
                    max_bytes: 8,
                },
                CancellationToken::new(),
            )
            .await;
            assert!(
                matches!(
                    result,
                    Err(SmartZipError::BackendProtocolError { .. })
                        | Err(SmartZipError::UnsafeArchivePath { .. })
                ),
                "{result:?}"
            );
        }
    }
}
