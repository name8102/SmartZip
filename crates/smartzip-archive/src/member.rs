//! Indexed member access, separate from filesystem path safety.
use crate::EntryRef;
use serde::{Deserialize, Serialize};
use smartzip_core::EncodingMode;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberReadRequest {
    pub archive: PathBuf,
    pub entry: EntryRef,
    pub password: Option<String>,
    pub encoding: EncodingMode,
    pub extent: ReadExtent,
    /// Source view used by embedded / selected volume catalogs.
    pub input: Option<smartzip_core::ArchiveInput>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ReadExtent {
    Complete { max_bytes: usize },
    Prefix { bytes: usize },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberReadResult {
    pub bytes: Vec<u8>,
    pub complete: bool,
}
