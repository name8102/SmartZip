//! Archive backend abstractions and concrete archive implementations.

pub mod backend;
pub mod diagnostic;
pub mod integrity;
pub mod native_zip;
pub mod router;
pub mod safety;
pub mod sevenzz;
mod test_output;
pub mod types;
pub mod unrar;
pub mod volume_probe;
pub mod volumes;

pub use backend::{ArchiveAdapter, ArchiveExecutor};
pub use native_zip::NativeZipBackend;
pub use router::{AdapterRegistration, BackendRouter};
pub use sevenzz::{
    SevenZipBackend, SevenZipDiagnosticSeverity, SevenZipEvent, SevenZipExitStatus,
    SevenZipLocator, SevenZipOperation, SevenZipReport, SevenZipResult,
};
pub use types::*;
pub use unrar::{UnrarBackend, UnrarLocator};
