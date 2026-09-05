//! Bounded, cancellable test-process output. Text supplies candidates; local
//! checksum readers supply confirmed physical-volume evidence.
use crate::integrity::{BackendTestDiagnostics, Coverage, TestFailure};
use crate::{BackendCommandOutput, TestResult};
use smartzip_core::{Result, SmartZipError};
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

pub(crate) const MAX_OUTPUT: usize = 16 * 1024 * 1024;

pub(crate) async fn bounded_read(
    mut stream: impl AsyncRead + Unpin,
) -> std::io::Result<(Vec<u8>, bool)> {
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

pub(crate) async fn run(
    executable: &Path,
    id: &str,
    args: &[String],
    token: &tokio_util::sync::CancellationToken,
) -> Result<(BackendCommandOutput, bool)> {
    if token.is_cancelled() {
        return Err(SmartZipError::Cancelled);
    }
    let mut command = Command::new(executable);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                SmartZipError::BackendUnavailable { backend: id.into() }
            } else {
                SmartZipError::io(Some(executable.to_path_buf()), source)
            }
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SmartZipError::BackendProtocolError {
            backend: id.into(),
            detail: "missing test stdout pipe".into(),
        })?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| SmartZipError::BackendProtocolError {
            backend: id.into(),
            detail: "missing test stderr pipe".into(),
        })?;
    // Dropping this future drops the child and both readers. No detached pipe
    // task can keep the process or an unbounded output buffer alive.
    let pid = child.id();
    let output = tokio::select! {
        result = async { tokio::try_join!(child.wait(), bounded_read(stdout), bounded_read(stderr)) } => result,
        _ = token.cancelled() => {
            #[cfg(unix)]
            if let Some(pid) = pid { unsafe { libc::kill(-(pid as i32), libc::SIGKILL); } }
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(SmartZipError::Cancelled);
        }
    };
    let (status, (stdout, cut_out), (stderr, cut_err)) =
        output.map_err(|source| SmartZipError::io(Some(executable.to_path_buf()), source))?;
    Ok((
        BackendCommandOutput {
            status: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        },
        cut_out || cut_err,
    ))
}

pub(crate) fn report(
    id: &str,
    family: &str,
    mut output: BackendCommandOutput,
    truncated: bool,
    password: Option<&str>,
) -> TestResult {
    let combined = format!("{}\n{}", output.stdout, output.stderr);
    let lower = combined.to_ascii_lowercase();
    let ok = output.status == Some(0);
    // Listing text also contains attacker-controlled filenames. A successful
    // process with a supplied password does not prove that it used that password.
    // The workflow verifies usage through a preceding password-required attempt.
    let encrypted = (ok && password.is_none_or(str::is_empty)).then_some(false);
    let failure = if ok {
        None
    } else if output.status == Some(255)
        || family == "unrar" && output.status == Some(10) && lower.contains("break")
    {
        Some(TestFailure::Cancelled)
    } else if lower.contains("missing volume")
        || lower.contains("cannot find volume")
        || lower.contains("cannot open next volume")
        || lower.contains("cannot find the file specified")
    {
        Some(TestFailure::MissingVolume)
    } else if lower.contains("password") {
        if password.is_none_or(str::is_empty) {
            Some(TestFailure::PasswordRequired)
        } else if lower.contains("wrong password?")
            || lower.contains("data error")
            || lower.contains("checksum")
        {
            Some(TestFailure::PasswordIndeterminate)
        } else {
            Some(TestFailure::PasswordRejected)
        }
    } else if lower.contains("permission denied")
        || lower.contains("access is denied")
        || lower.contains("cannot open") && !lower.contains("as archive")
    {
        Some(TestFailure::Io)
    } else if lower.contains("crc failed")
        || lower.contains("crc error")
        || lower.contains("checksum error")
        || lower.contains("data error")
        || lower.contains("headers error")
        || lower.contains("unexpected end")
        || lower.contains("corrupt")
    {
        Some(TestFailure::Corruption)
    } else {
        Some(TestFailure::Unknown)
    };
    let mut damaged_files: Vec<String> = combined
        .lines()
        .filter_map(|line| {
            // Names are untrusted hints. They never establish a volume checksum.
            [
                "ERROR: CRC Failed : ",
                "ERROR: Data Error : ",
                "ERROR: CRC Failed in encrypted file. Wrong password? : ",
            ]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
            .map(str::to_owned)
        })
        .take(4096)
        .collect();
    let mut version: Option<String> = combined
        .lines()
        .find(|line| line.starts_with("7-Zip ") || line.starts_with("UNRAR "))
        .map(|s| s.chars().take(160).collect());
    if let Some(password) = password.filter(|p| !p.is_empty()) {
        output.stdout = output.stdout.replace(password, "[redacted]");
        output.stderr = output.stderr.replace(password, "[redacted]");
        for name in &mut damaged_files {
            *name = name.replace(password, "[redacted]");
        }
        version = version.map(|value| value.replace(password, "[redacted]"));
    }
    TestResult {
        ok,
        encrypted,
        diagnostics: BackendTestDiagnostics {
            adapter_id: id.into(),
            family: family.into(),
            version,
            exit_code: output.status,
            failure,
            coverage: if ok {
                Coverage::Complete
            } else {
                Coverage::Partial
            },
            damaged_files,
            stdout: output.stdout,
            stderr: output.stderr,
            output_truncated: truncated,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_output_keeps_failure_and_does_not_claim_a_volume() {
        let result = report(
            "7z",
            "7z",
            BackendCommandOutput {
                status: Some(2),
                stdout: "Testing archive: a.7z.001\n".into(),
                stderr: "ERROR: Data Error : file\n".into(),
            },
            false,
            None,
        );
        assert!(!result.ok);
        assert_eq!(result.diagnostics.failure, Some(TestFailure::Corruption));
        assert_eq!(result.diagnostics.damaged_files, ["file"]);
        assert_eq!(result.diagnostics.coverage, Coverage::Partial);
    }

    #[test]
    fn missing_volume_precedes_cascading_crc_and_password_ambiguity_is_retained() {
        let run = |stderr: &str| {
            report(
                "unrar",
                "unrar",
                BackendCommandOutput {
                    status: Some(3),
                    stdout: String::new(),
                    stderr: stderr.into(),
                },
                false,
                Some("secret"),
            )
        };
        assert_eq!(
            run("Cannot find volume a.part2.rar\nchecksum error")
                .diagnostics
                .failure,
            Some(TestFailure::MissingVolume)
        );
        let ambiguous = run("Data error in encrypted file. Wrong password? secret");
        assert_eq!(
            ambiguous.diagnostics.failure,
            Some(TestFailure::PasswordIndeterminate)
        );
        assert!(!ambiguous.diagnostics.stderr.contains("secret"));
    }

    #[test]
    fn untrusted_names_do_not_prove_password_usage_and_all_output_is_redacted() {
        let output = |status| BackendCommandOutput {
            status: Some(status),
            stdout: "7-Zip secret\nTesting password 7zAES Encrypted = +\n".into(),
            stderr: "ERROR: CRC Failed : secret\n".into(),
        };
        let success = report("7z", "7z", output(0), false, Some("secret"));
        assert_eq!(success.encrypted, None);
        let failure = report("7z", "7z", output(2), false, Some("secret"));
        assert_eq!(
            failure.diagnostics.version.as_deref(),
            Some("7-Zip [redacted]")
        );
        assert_eq!(failure.diagnostics.damaged_files, ["[redacted]"]);
        assert!(!failure.diagnostics.stdout.contains("secret"));
        assert!(!failure.diagnostics.stderr.contains("secret"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancelling_run_kills_the_child_process() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("pid");
        let args = vec![
            "-c".into(),
            "echo $$ > \"$1\"; exec sleep 60".into(),
            "test".into(),
            pid_path.to_string_lossy().into_owned(),
        ];
        let token = tokio_util::sync::CancellationToken::new();
        let child_token = token.clone();
        let pending =
            tokio::spawn(
                async move { run(Path::new("/bin/sh"), "test", &args, &child_token).await },
            );
        let pid = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(&pid_path) {
                    if let Ok(pid) = pid.trim().parse::<u32>() {
                        break pid;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        token.cancel();
        assert!(matches!(
            pending.await.unwrap(),
            Err(SmartZipError::Cancelled)
        ));
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while Path::new(&format!("/proc/{pid}")).exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancelled test subprocess was not reaped");
    }
}

pub(crate) async fn collect_bounded_output(
    task: Option<tokio::task::JoinHandle<std::io::Result<(Vec<u8>, bool)>>>,
) -> Result<Vec<u8>> {
    let Some(task) = task else {
        return Ok(Vec::new());
    };
    let (bytes, truncated) = task
        .await
        .map_err(|e| SmartZipError::io(None, std::io::Error::other(e)))?
        .map_err(|e| SmartZipError::io(None, e))?;
    if truncated {
        return Err(SmartZipError::ResourceLimit {
            detail: "backend output exceeded 16 MiB; incomplete output was rejected".into(),
        });
    }
    Ok(bytes)
}
