pub mod rar;
pub mod sevenzip;
pub mod zip;

use smartzip_core::ArchiveFormat;
use std::path::Path;

/// Cheap static structural probe for split-volume archives.
///
/// This lives in `smartzip-archive` and knows only archive format facts.
/// It must not perform directory enumeration, filename clustering, or full
/// backend `test` operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeProbeResult {
    /// Not a supported multivolume family or not enough evidence.
    NotApplicable,
    /// Strong evidence the file is standalone (no cross-file needed).
    Standalone(ArchiveFormat),
    /// Strong evidence the file is part of a multivolume set.
    MultiVolume(VolumeStructure),
    /// Weak/possible multivolume evidence, needs filename hypothesis support.
    PossiblyMultiVolume(VolumeStructure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeStructure {
    pub format: ArchiveFormat,
    /// Internal logical volume index (0-based for 7z/ZIP, 1-based for RAR parts may vary).
    pub logical_volume_index: Option<u32>,
    pub expected_volume_count: Option<u32>,
    /// Expected logical archive size when determinable (e.g., 7z NextHeader extent).
    pub expected_logical_size: Option<u64>,
    pub is_last_volume: Option<bool>,
}

/// Probe a physical file for volume structure without invoking backends.
pub fn probe_volume_structure(path: &Path) -> VolumeProbeResult {
    if let Some(res) = rar::probe_rar(path) {
        return res;
    }
    if let Some(res) = zip::probe_zip(path) {
        return res;
    }
    if let Some(res) = sevenzip::probe_7z(path) {
        return res;
    }
    VolumeProbeResult::NotApplicable
}
