//! Physical archive data and logical volume names passed to native readers.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveInput {
    pub path: PathBuf,
    pub offset: u64,
    pub length: Option<u64>,
    pub volumes: BTreeMap<String, PathBuf>,
}

/// Admission before an archive backend creates entries or writes output bytes.
pub trait OutputBudget: Send + Sync {
    fn reserve(&self, files: u64, bytes: u64) -> crate::Result<()>;

    /// Select write admission or external-backend tree monitoring for this attempt.
    fn set_native_admission(&self, _enabled: bool) {}

    /// Release failed-backend usage only after its private output has been removed.
    fn reset_attempt_usage(&self) -> crate::Result<()> {
        Ok(())
    }

    /// Unlimited attempts avoid a parent/worker round trip for every write.
    fn requires_admission(&self) -> bool {
        true
    }
}
