//! Password candidate generation and ranking.

use serde::{Deserialize, Serialize};
use smartzip_db::password::{NewPassword, PasswordRecord, PasswordRepository};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PasswordSource {
    Empty,
    Manual,
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
    pub include_empty: bool,
}

impl Default for PasswordCandidateRequest {
    fn default() -> Self {
        Self {
            manual: Vec::new(),
            include_empty: true,
        }
    }
}

pub struct PasswordService<'a> {
    repo: Option<PasswordRepository<'a>>,
    policy: smartzip_config::Passwords,
}

impl<'a> PasswordService<'a> {
    pub fn new(repo: PasswordRepository<'a>) -> Self {
        Self::configured(
            Some(repo),
            smartzip_config::Passwords::default(),
            smartzip_config::StateMode::ReadWrite,
        )
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
            policy,
        }
    }

    pub fn remember_batch(&self, batch: &mut Vec<PasswordCandidate>, value: &str, id: Option<i64>) {
        if !self
            .policy
            .sources
            .contains(&smartzip_config::PasswordSource::Batch)
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
        self.policy.mode != smartzip_config::PasswordMode::Off
            && self
                .policy
                .sources
                .contains(&smartzip_config::PasswordSource::Manual)
    }

    pub fn order_candidates(
        &self,
        base: &[PasswordCandidate],
        known: Option<&PasswordCandidate>,
        batch: &[PasswordCandidate],
    ) -> Vec<PasswordCandidate> {
        let policy = &self.policy;
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
                    .sources
                    .contains(&smartzip_config::PasswordSource::Manual)
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

        if request.include_empty
            && self
                .policy
                .sources
                .contains(&smartzip_config::PasswordSource::Empty)
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

        if self
            .policy
            .sources
            .contains(&smartzip_config::PasswordSource::Database)
        {
            if let Some(repo) = &self.repo {
                for record in repo.ranked_candidates(self.policy.database_limit)? {
                    push_unique(&mut candidates, candidate_from_record(record));
                }
            }
        }

        Ok(self.order_candidates(&candidates, None, &[]))
    }

    pub fn record_success(
        &self,
        candidate: &PasswordCandidate,
    ) -> smartzip_db::Result<Option<i64>> {
        let existing = if candidate.id.is_none()
            && (!self.policy.save_success && self.policy.record_statistics)
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
                && self.policy.save_success =>
            {
                self.add_password(&candidate.value, "auto", false)?
            }
            None => return Ok(None),
        };
        if self.policy.record_statistics {
            if let Some(repo) = &self.repo {
                repo.record_success(id)?;
            }
        }
        Ok(Some(id))
    }

    pub fn record_failure(&self, candidate: &PasswordCandidate) -> smartzip_db::Result<()> {
        if self.policy.record_statistics {
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
        if !self
            .policy
            .sources
            .contains(&smartzip_config::PasswordSource::Known)
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

fn candidate_from_record(record: PasswordRecord) -> PasswordCandidate {
    PasswordCandidate {
        id: Some(record.id),
        value: record.value,
        source: PasswordSource::Database,
    }
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
                include_empty: true,
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
    fn configured_database_limit_applies_without_limiting_manual_candidates() {
        let db = smartzip_db::SmartZipDb::in_memory().unwrap();
        let service = PasswordService::configured(
            Some(PasswordRepository::new(db.connection())),
            Passwords {
                database_limit: 1,
                ..Default::default()
            },
            StateMode::ReadWrite,
        );
        service
            .add_password("stored-first", "manual", false)
            .unwrap();
        service
            .add_password("stored-second", "manual", false)
            .unwrap();
        let candidates = service
            .ranked_candidates(PasswordCandidateRequest {
                manual: vec!["explicit-first".into(), "explicit-second".into()],
                include_empty: false,
            })
            .unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|c| c.value.as_str())
                .collect::<Vec<_>>(),
            ["explicit-first", "explicit-second", "stored-first"]
        );
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
