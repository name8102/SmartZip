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
            limit: 128,
        }
    }
}

pub struct PasswordService<'a> {
    repo: PasswordRepository<'a>,
}

impl<'a> PasswordService<'a> {
    pub fn new(repo: PasswordRepository<'a>) -> Self {
        Self { repo }
    }

    pub fn add_password(
        &self,
        value: &str,
        source: &str,
        pinned: bool,
    ) -> smartzip_db::Result<i64> {
        self.repo.upsert(NewPassword {
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

        if request.include_empty {
            push_unique(
                &mut candidates,
                PasswordCandidate {
                    id: None,
                    value: String::new(),
                    source: PasswordSource::Empty,
                },
            );
        }

        for value in request
            .manual
            .into_iter()
            .map(normalize_password)
            .filter(|v| !v.is_empty())
        {
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

        for record in self.repo.ranked_candidates(request.limit)? {
            push_unique(&mut candidates, candidate_from_record(record));
            if candidates.len() >= request.limit {
                break;
            }
        }

        Ok(candidates)
    }

    pub fn record_success(
        &self,
        candidate: &PasswordCandidate,
    ) -> smartzip_db::Result<Option<i64>> {
        let id = match candidate.id {
            Some(id) => id,
            None if !candidate.value.is_empty() => {
                self.add_password(&candidate.value, "auto", false)?
            }
            None => return Ok(None),
        };
        self.repo.record_success(id)?;
        Ok(Some(id))
    }

    pub fn record_failure(&self, candidate: &PasswordCandidate) -> smartzip_db::Result<()> {
        if let Some(id) = candidate.id {
            self.repo.record_failure(id)?;
        }
        Ok(())
    }
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
    fn candidates_include_empty_manual_clipboard_and_db_without_duplicates() {
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

        assert_eq!(candidates[0].source, PasswordSource::Empty);
        assert!(candidates.iter().any(|c| c.value == "手动密码"));
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
}
