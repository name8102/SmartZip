//! Archive backend abstractions and concrete archive implementations.

pub mod backend;
pub mod native_zip;
pub mod router;
pub mod sevenzz;
pub mod types;
pub mod unrar;
pub mod zip;

pub use backend::ArchiveBackend;
pub use native_zip::NativeZipBackend;
pub use router::BackendRouter;
pub use sevenzz::{SevenZipBackend, SevenZipLocator};
pub use types::*;
pub use unrar::{UnrarBackend, UnrarLocator};
pub use zip::ZipBackend;
