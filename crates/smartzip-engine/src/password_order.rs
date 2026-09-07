//! Password candidate load/order/remember helpers.

use smartzip_passwords::{PasswordCandidate, PasswordCandidateRequest, PasswordService};

pub(crate) fn load_password_candidates(
    passwords: &PasswordService<'_>,
    request: PasswordCandidateRequest,
) -> smartzip_core::Result<Vec<PasswordCandidate>> {
    passwords.ranked_candidates(request).map_err(|error| {
        smartzip_core::SmartZipError::BackendFailed {
            backend: "password-db".into(),
            exit_code: None,
            stderr: error.to_string(),
        }
    })
}

pub(crate) fn password_source_label(candidate: &PasswordCandidate) -> &'static str {
    match candidate.source {
        smartzip_passwords::PasswordSource::Empty => "empty",
        smartzip_passwords::PasswordSource::Manual => "manual",
        smartzip_passwords::PasswordSource::Clipboard => "clipboard",
        smartzip_passwords::PasswordSource::Recent => "recent",
        smartzip_passwords::PasswordSource::Database => "database",
    }
}
