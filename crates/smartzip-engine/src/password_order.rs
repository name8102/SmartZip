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

pub(crate) fn password_value(candidate: &PasswordCandidate) -> Option<String> {
    Some(candidate.value.clone())
}

pub(crate) fn order_password_candidates(
    base: &[PasswordCandidate],
    known: Option<&PasswordCandidate>,
    batch: &[PasswordCandidate],
) -> Vec<PasswordCandidate> {
    let mut ordered = Vec::with_capacity(base.len() + batch.len() + usize::from(known.is_some()));

    for candidate in base.iter().filter(|candidate| {
        matches!(
            candidate.source,
            smartzip_passwords::PasswordSource::Manual
                | smartzip_passwords::PasswordSource::Clipboard
        )
    }) {
        push_password_unique(&mut ordered, candidate.clone());
    }
    if let Some(candidate) = known {
        push_password_unique(&mut ordered, candidate.clone());
    }
    for candidate in batch {
        push_password_unique(&mut ordered, candidate.clone());
    }
    for candidate in base.iter().filter(|candidate| {
        !matches!(
            candidate.source,
            smartzip_passwords::PasswordSource::Manual
                | smartzip_passwords::PasswordSource::Clipboard
        )
    }) {
        push_password_unique(&mut ordered, candidate.clone());
    }

    ordered
}

pub(crate) fn remember_batch_password(
    batch: &mut Vec<PasswordCandidate>,
    value: &str,
    id: Option<i64>,
) {
    if batch.iter().any(|candidate| candidate.value == value) {
        return;
    }
    batch.push(PasswordCandidate {
        id,
        value: value.to_string(),
        source: smartzip_passwords::PasswordSource::Recent,
    });
}

pub(crate) fn push_password_unique(
    ordered: &mut Vec<PasswordCandidate>,
    candidate: PasswordCandidate,
) {
    if !ordered
        .iter()
        .any(|existing| existing.value == candidate.value)
    {
        ordered.push(candidate);
    }
}

/// Human-readable label for an [`EncodingMode`], used for the

pub(crate) fn password_source_label(candidate: &PasswordCandidate) -> &'static str {
    match candidate.source {
        smartzip_passwords::PasswordSource::Empty => "empty",
        smartzip_passwords::PasswordSource::Manual => "manual",
        smartzip_passwords::PasswordSource::Clipboard => "clipboard",
        smartzip_passwords::PasswordSource::Recent => "recent",
        smartzip_passwords::PasswordSource::Database => "database",
    }
}

pub(crate) fn password_attempt_index(
    candidate: &PasswordCandidate,
    candidates: &[PasswordCandidate],
) -> usize {
    candidates
        .iter()
        .position(|existing| existing == candidate)
        .map(|idx| idx + 1)
        .unwrap_or(0)
}
