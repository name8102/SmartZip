//! Archive backend abstractions and concrete archive implementations.

pub mod backend;
pub mod router;
pub mod sevenzz;
pub mod types;
pub mod unrar;
pub mod zip;

pub use backend::ArchiveBackend;
pub use router::BackendRouter;
pub use sevenzz::{SevenZipBackend, SevenZipLocator};
pub use types::*;
pub use unrar::{UnrarBackend, UnrarLocator};
pub use zip::ZipBackend;
