//! Bounded, cancellable test-process output. Text supplies candidates; local
//! checksum readers supply confirmed physical-volume evidence.
use crate::integrity::{BackendTestDiagnostics, Coverage, TestFailure};
use crate::{BackendCommandOutput, TestResult};
use smartzip_core::{Result, SmartZipError};
use std::path::Path;

pub(crate) async fn run(
    executable: &Path,
    id: &str,
    args: &[String],
    token: &tokio_util::sync::CancellationToken,
) -> Result<(BackendCommandOutput, bool)> {
    crate::process::run_bounded(
        executable,
        id,
        args,
        token,
        crate::process::Mode::Diagnostic,
    )
    .await
}

/// Only diagnostic lines participate in classification, never displayed names.
pub(crate) fn diagnostic_text(combined: &str, family: &str) -> String {
    combined
        .lines()
        .filter_map(|line| {
            let line = line.trim().to_ascii_lowercase();
            let message = line.strip_prefix("error: ").unwrap_or(&line);
            let message = if family == "unrar" {
                message.rsplit_once(" - ").map_or(message, |(_, tail)| tail)
            } else {
                message
            };
            const PREFIXES: &[&str] = &[
                "missing volume",
                "cannot find volume",
                "cannot open next volume",
                "cannot find the file specified",
                "cannot open",
                "can not open",
                "wrong password",
                "incorrect password",
                "password is incorrect",
                "password required",
                "password is required",
                "enter password",
                "unsupported method",
                "unknown method",
                "unsupported archive",
                "is not archive",
                "no such file",
                "the system cannot find",
                "file not found",
                "data error",
                "crc failed",
                "crc error",
                "checksum error",
                "headers error",
                "unexpected end",
                "corrupt",
                "permission denied",
                "access is denied",
                "user break",
                "break signaled",
            ];
            let is_diagnostic = |message: &str| {
                PREFIXES.iter().any(|prefix| {
                    message.strip_prefix(prefix).is_some_and(|rest| {
                        rest.is_empty() || rest.starts_with([' ', '?', ':']) || rest == "."
                    })
                })
            };
            let first = message.split(" : ").next().unwrap_or(message);
            if is_diagnostic(first) {
                return Some(first.to_owned());
            }
            // 7z listing failures put the archive path before the diagnostic:
            // ERROR: path : Can not open encrypted archive. Wrong password?
            if line.starts_with("error: ") {
                let last = message.rsplit(" : ").next().unwrap_or(message);
                if is_diagnostic(last) {
                    return Some(last.to_owned());
                }
            }
            None
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn password_error(
    output: &BackendCommandOutput,
    family: &str,
    password: Option<&str>,
    path: &Path,
) -> Option<SmartZipError> {
    let failure = report(
        "credential-attempt",
        family,
        output.clone(),
        false,
        password,
    )
    .diagnostics
    .failure;
    match failure {
        Some(TestFailure::PasswordRequired) => {
            Some(SmartZipError::PasswordRequired { path: path.into() })
        }
        Some(TestFailure::PasswordRejected) => {
            Some(SmartZipError::WrongPassword { path: path.into() })
        }
        Some(TestFailure::PasswordIndeterminate) => {
            Some(SmartZipError::PasswordIndeterminate { path: path.into() })
        }
        _ => None,
    }
}

pub(crate) fn report(
    id: &str,
    family: &str,
    mut output: BackendCommandOutput,
    truncated: bool,
    password: Option<&str>,
) -> TestResult {
    let combined = format!("{}\n{}", output.stdout, output.stderr);
    let lower = diagnostic_text(&combined, family);
    let password_failure = lower.lines().any(|message| {
        [
            "wrong password",
            "incorrect password",
            "password is incorrect",
            "password required",
            "password is required",
            "enter password",
            "cannot open encrypted archive",
            "can not open encrypted archive",
            "data error in encrypted file",
            "crc failed in encrypted file",
            "checksum error in the encrypted file",
        ]
        .iter()
        .any(|prefix| message.starts_with(prefix))
    });
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
    } else if password_failure {
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
    fn archive_and_entry_names_do_not_change_failure_classification() {
        for name in ["password.zip", "missing volume.zip", "corrupt.zip"] {
            let result = report(
                "7z",
                "7z",
                BackendCommandOutput {
                    status: Some(2),
                    stdout: format!("Testing archive: {name}\nPath = {name}\n"),
                    stderr: format!("ERROR: CRC Failed : {name}\n"),
                },
                false,
                None,
            );
            assert_eq!(result.diagnostics.failure, Some(TestFailure::Corruption));
            assert_eq!(result.diagnostics.damaged_files, [name]);
        }
        let result = report(
            "7z",
            "7z",
            BackendCommandOutput {
                status: Some(2),
                stdout: "Testing archive: /tmp/password/archive.jpg\n".into(),
                stderr: "ERROR: /tmp/password/archive.jpg\nCannot open the file as archive\n"
                    .into(),
            },
            false,
            None,
        );
        assert_eq!(result.diagnostics.failure, Some(TestFailure::Unknown));
        let io = report(
            "7z",
            "7z",
            BackendCommandOutput {
                status: Some(2),
                stdout: String::new(),
                stderr: "Cannot open /tmp/password/archive.jpg\nPermission denied\n".into(),
            },
            false,
            None,
        );
        assert_eq!(io.diagnostics.failure, Some(TestFailure::Io));
    }

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
