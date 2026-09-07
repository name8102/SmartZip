//! Password candidate generation and ranking.

use serde::{Deserialize, Serialize};
use smartzip_db::password::{NewPassword, PasswordRecord, PasswordRepository};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PasswordSource {
    Empty,
    Manual,
    Clipboard,
    Recent,
    Database,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasswordCandidate {
    pub id: Option<i64>,
    pub value: String,
    pub source: PasswordSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasswordCandidateRequest {
    pub manual: Vec<String>,
    pub clipboard: Option<String>,
    pub include_empty: bool,
    pub limit: usize,
}

impl Default for PasswordCandidateRequest {
    fn default() -> Self {
        Self {
            manual: Vec::new(),
            clipboard: None,
            include_empty: true,
            limit: smartzip_config::DEFAULT_PASSWORD_LIMIT,
        }
    }
}

pub struct PasswordService<'a> {
    repo: Option<PasswordRepository<'a>>,
    policy: Option<smartzip_config::Passwords>,
}

impl<'a> PasswordService<'a> {
    pub fn new(repo: PasswordRepository<'a>) -> Self {
        Self {
            repo: Some(repo),
            policy: None,
        }
    }

    pub fn configured(
        repo: Option<PasswordRepository<'a>>,
        mut policy: smartzip_config::Passwords,
        state: smartzip_config::StateMode,
    ) -> Self {
        use smartzip_config::{PasswordMode, PasswordSource as Source, StateMode};
        if state != StateMode::ReadWrite {
            policy.save_success = false;
            policy.record_statistics = false;
        }
        policy.sources.retain(|source| match policy.mode {
            PasswordMode::Off => false,
            PasswordMode::Manual => matches!(source, Source::Manual | Source::Empty),
            PasswordMode::Auto => {
                state != StateMode::Off || !matches!(source, Source::Known | Source::Database)
            }
        });
        Self {
            repo: if state == StateMode::Off { None } else { repo },
            policy: Some(policy),
        }
    }

    pub fn remember_batch(&self, batch: &mut Vec<PasswordCandidate>, value: &str, id: Option<i64>) {
        if self
            .policy
            .as_ref()
            .is_some_and(|p| !p.sources.contains(&smartzip_config::PasswordSource::Batch))
        {
            return;
        }
        if !batch.iter().any(|candidate| candidate.value == value) {
            batch.push(PasswordCandidate {
                id,
                value: value.into(),
                source: PasswordSource::Recent,
            });
        }
    }

    pub fn allows_prompt(&self) -> bool {
        self.policy.as_ref().is_none_or(|p| {
            p.mode != smartzip_config::PasswordMode::Off
                && p.sources.contains(&smartzip_config::PasswordSource::Manual)
        })
    }

    pub fn order_candidates(
        &self,
        base: &[PasswordCandidate],
        known: Option<&PasswordCandidate>,
        batch: &[PasswordCandidate],
    ) -> Vec<PasswordCandidate> {
        let Some(policy) = &self.policy else {
            return legacy_order(base, known, batch);
        };
        use smartzip_config::PasswordSource as Source;
        let mut ordered = Vec::new();
        for source in &policy.sources {
            match source {
                Source::Known => {
                    if let Some(candidate) = known {
                        push_unique(&mut ordered, candidate.clone());
                    }
                }
                Source::Batch => {
                    for candidate in batch {
                        push_unique(&mut ordered, candidate.clone());
                    }
                }
                _ => {
                    for candidate in base.iter().filter(|c| {
                        matches!(
                            (source, &c.source),
                            (Source::Manual, PasswordSource::Manual)
                                | (Source::Empty, PasswordSource::Empty)
                                | (Source::Database, PasswordSource::Database)
                        )
                    }) {
                        push_unique(&mut ordered, candidate.clone());
                    }
                }
            }
        }
        // This is an unauthenticated operation, not an implicit credential source.
        if policy.mode == smartzip_config::PasswordMode::Off {
            ordered.push(PasswordCandidate {
                id: None,
                value: String::new(),
                source: PasswordSource::Empty,
            });
        }
        ordered
    }

    pub fn add_password(
        &self,
        value: &str,
        source: &str,
        pinned: bool,
    ) -> smartzip_db::Result<i64> {
        self.repo
            .as_ref()
            .ok_or_else(|| std::io::Error::other("password storage disabled"))?
            .upsert(NewPassword {
                value,
                source,
                pinned,
            })
    }

    pub fn ranked_candidates(
        &self,
        request: PasswordCandidateRequest,
    ) -> smartzip_db::Result<Vec<PasswordCandidate>> {
        let mut candidates = Vec::new();

        for value in request.manual.into_iter().filter(|v| {
            !v.is_empty()
                && self
                    .policy
                    .as_ref()
                    .is_none_or(|p| p.sources.contains(&smartzip_config::PasswordSource::Manual))
        }) {
            push_unique(
                &mut candidates,
                PasswordCandidate {
                    id: None,
                    value,
                    source: PasswordSource::Manual,
                },
            );
        }

        if let Some(value) = request
            .clipboard
            .map(normalize_password)
            .filter(|v| !v.is_empty())
        {
            push_unique(
                &mut candidates,
                PasswordCandidate {
                    id: None,
                    value,
                    source: PasswordSource::Clipboard,
                },
            );
        }

        if request.include_empty
            && self
                .policy
                .as_ref()
                .is_none_or(|p| p.sources.contains(&smartzip_config::PasswordSource::Empty))
        {
            push_unique(
                &mut candidates,
                PasswordCandidate {
                    id: None,
                    value: String::new(),
                    source: PasswordSource::Empty,
                },
            );
        }

        if self.policy.as_ref().is_none_or(|p| {
            p.sources
                .contains(&smartzip_config::PasswordSource::Database)
        }) {
            if let Some(repo) = &self.repo {
                for record in repo.ranked_candidates(request.limit)? {
                    push_unique(&mut candidates, candidate_from_record(record));
                }
            }
        }

        if self.policy.is_some() {
            return Ok(self.order_candidates(&candidates, None, &[]));
        }
        Ok(candidates)
    }

    pub fn record_listing_access(
        &self,
        candidate: &PasswordCandidate,
    ) -> smartzip_db::Result<Option<i64>> {
        // A successful listing does not authenticate encrypted file contents.
        if self.policy.is_some() {
            Ok(candidate.id)
        } else {
            self.record_success(candidate)
        }
    }

    pub fn record_success(
        &self,
        candidate: &PasswordCandidate,
    ) -> smartzip_db::Result<Option<i64>> {
        let existing = if candidate.id.is_none()
            && self
                .policy
                .as_ref()
                .is_some_and(|p| !p.save_success && p.record_statistics)
        {
            self.repo
                .as_ref()
                .map(|repo| repo.get_by_value(&candidate.value))
                .transpose()?
                .flatten()
                .map(|r| r.id)
        } else {
            candidate.id
        };
        let id = match existing {
            Some(id) => id,
            None if !candidate.value.is_empty()
                && self.repo.is_some()
                && self.policy.as_ref().is_none_or(|p| p.save_success) =>
            {
                self.add_password(&candidate.value, "auto", false)?
            }
            None => return Ok(None),
        };
        if self.policy.as_ref().is_none_or(|p| p.record_statistics) {
            if let Some(repo) = &self.repo {
                repo.record_success(id)?;
            }
        }
        Ok(Some(id))
    }

    pub fn record_failure(&self, candidate: &PasswordCandidate) -> smartzip_db::Result<()> {
        if self.policy.as_ref().is_none_or(|p| p.record_statistics) {
            if let (Some(repo), Some(id)) = (&self.repo, candidate.id) {
                repo.record_failure(id)?;
            }
        }
        Ok(())
    }

    /// Build a candidate from a stored password id, or `None` if the id is
    /// unknown or the row is disabled. Used to inject the `known_files`-matched
    /// password at the top of the try order for a specific file.
    pub fn candidate_by_id(&self, id: i64) -> smartzip_db::Result<Option<PasswordCandidate>> {
        if self
            .policy
            .as_ref()
            .is_some_and(|p| !p.sources.contains(&smartzip_config::PasswordSource::Known))
        {
            return Ok(None);
        }
        let Some(repo) = &self.repo else {
            return Ok(None);
        };
        Ok(repo
            .get_by_id(id)?
            .and_then(|record| (!record.disabled).then(|| candidate_from_record(record))))
    }
}

fn legacy_order(
    base: &[PasswordCandidate],
    known: Option<&PasswordCandidate>,
    batch: &[PasswordCandidate],
) -> Vec<PasswordCandidate> {
    let mut ordered = Vec::new();
    for candidate in base
        .iter()
        .filter(|c| matches!(c.source, PasswordSource::Manual | PasswordSource::Clipboard))
    {
        push_unique(&mut ordered, candidate.clone());
    }
    if let Some(candidate) = known {
        push_unique(&mut ordered, candidate.clone());
    }
    for candidate in batch {
        push_unique(&mut ordered, candidate.clone());
    }
    for candidate in base {
        push_unique(&mut ordered, candidate.clone());
    }
    ordered
}

fn candidate_from_record(record: PasswordRecord) -> PasswordCandidate {
    PasswordCandidate {
        id: Some(record.id),
        value: record.value,
        source: PasswordSource::Database,
    }
}

fn normalize_password(value: String) -> String {
    value
        .trim_matches(|ch: char| ch.is_whitespace() || ch == '\u{0}')
        .to_string()
}

fn push_unique(candidates: &mut Vec<PasswordCandidate>, candidate: PasswordCandidate) {
    if !candidates
        .iter()
        .any(|existing| existing.value == candidate.value)
    {
        candidates.push(candidate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartzip_db::{password::PasswordRepository, SmartZipDb};

    #[test]
    fn candidates_order_explicit_before_empty_and_database_fallback() {
        let db = SmartZipDb::in_memory().unwrap();
        let service = PasswordService::new(PasswordRepository::new(db.connection()));
        service.add_password("数据库密码", "manual", false).unwrap();
        service.add_password("剪贴板密码", "manual", false).unwrap();

        let candidates = service
            .ranked_candidates(PasswordCandidateRequest {
                manual: vec![" 手动密码\n".into()],
                clipboard: Some("剪贴板密码".into()),
                include_empty: true,
                limit: 10,
            })
            .unwrap();

        assert_eq!(candidates[0].value, " 手动密码\n");
        assert_eq!(candidates[0].source, PasswordSource::Manual);
        assert_eq!(candidates[1].value, "剪贴板密码");
        assert_eq!(candidates[1].source, PasswordSource::Clipboard);
        assert_eq!(candidates[2].source, PasswordSource::Empty);
        assert_eq!(
            candidates
                .iter()
                .filter(|c| c.value == "剪贴板密码")
                .count(),
            1
        );
        assert!(candidates.iter().any(|c| c.value == "数据库密码"));
    }

    #[test]
    fn success_auto_saves_manual_candidate() {
        let db = SmartZipDb::in_memory().unwrap();
        let service = PasswordService::new(PasswordRepository::new(db.connection()));
        let id = service
            .record_success(&PasswordCandidate {
                id: None,
                value: "新密码".into(),
                source: PasswordSource::Manual,
            })
            .unwrap()
            .unwrap();
        assert!(id > 0);
    }

    #[test]
    fn manual_passwords_keep_database_fallback_after_explicit_candidates() {
        let db = SmartZipDb::in_memory().unwrap();
        let service = PasswordService::new(PasswordRepository::new(db.connection()));
        service.add_password("数据库密码", "manual", false).unwrap();

        let candidates = service
            .ranked_candidates(PasswordCandidateRequest {
                manual: vec!["手动密码".into()],
                clipboard: None,
                include_empty: true,
                limit: 10,
            })
            .unwrap();

        assert_eq!(candidates[0].value, "手动密码");
        assert!(candidates.iter().any(|c| c.value.is_empty()));
        assert!(candidates.iter().any(|c| c.value == "数据库密码"));
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use smartzip_config::{PasswordSource as Source, Passwords, StateMode};

    #[test]
    fn disabled_password_storage_does_not_query_or_write_the_repository() {
        let db = smartzip_db::SmartZipDb::in_memory().unwrap();
        db.connection()
            .execute_batch("DROP TABLE passwords")
            .unwrap();
        let service = PasswordService::configured(
            Some(PasswordRepository::new(db.connection())),
            Passwords {
                sources: vec![Source::Manual, Source::Batch, Source::Empty],
                save_success: false,
                record_statistics: false,
                ..Default::default()
            },
            StateMode::ReadWrite,
        );
        let candidates = service
            .ranked_candidates(PasswordCandidateRequest {
                manual: vec!["sentinel".into()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(service.record_success(&candidates[0]).unwrap(), None);
        service
            .record_failure(&PasswordCandidate {
                id: Some(1),
                ..candidates[0].clone()
            })
            .unwrap();
        assert!(service.candidate_by_id(1).unwrap().is_none());
    }

    #[test]
    fn saving_and_statistics_are_independent_and_source_order_is_exact() {
        let db = smartzip_db::SmartZipDb::in_memory().unwrap();
        let service = PasswordService::configured(
            Some(PasswordRepository::new(db.connection())),
            Passwords {
                record_statistics: false,
                ..Default::default()
            },
            StateMode::ReadWrite,
        );
        let candidate = PasswordCandidate {
            id: None,
            value: "new".into(),
            source: PasswordSource::Manual,
        };
        let id = service.record_success(&candidate).unwrap().unwrap();
        let repo = PasswordRepository::new(db.connection());
        assert_eq!(repo.get_by_id(id).unwrap().unwrap().success_count, 0);
        let service = PasswordService::configured(
            Some(PasswordRepository::new(db.connection())),
            Passwords {
                save_success: false,
                sources: vec![Source::Database, Source::Manual],
                ..Default::default()
            },
            StateMode::ReadWrite,
        );
        assert_eq!(service.record_success(&candidate).unwrap(), Some(id));
        assert_eq!(repo.get_by_id(id).unwrap().unwrap().success_count, 1);
        let candidates = service
            .ranked_candidates(PasswordCandidateRequest {
                manual: vec!["explicit".into()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|c| c.value.as_str())
                .collect::<Vec<_>>(),
            ["new", "explicit"]
        );
        let batch = PasswordCandidate {
            id: None,
            value: "batch".into(),
            source: PasswordSource::Recent,
        };
        assert!(!service
            .order_candidates(&candidates, None, &[batch])
            .iter()
            .any(|c| c.value == "batch"));
    }
}
