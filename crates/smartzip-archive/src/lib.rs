//! Archive backend abstractions and concrete archive implementations.

pub mod backend;
pub mod diagnostic;
pub mod integrity;
mod locator;
pub mod member;
pub mod native_zip;
mod process;
pub mod router;
pub mod safety;
pub mod sevenzip;
pub mod source;
mod test_output;
pub mod types;
pub mod unrar;
pub mod volume_probe;
pub mod volumes;
#[doc(hidden)]
pub mod worker;

pub use backend::{ArchiveAdapter, ArchiveExecutor};
pub use member::{MemberReadRequest, MemberReadResult, ReadExtent};
pub use native_zip::NativeZipBackend;
pub use router::{AdapterRegistration, BackendRouter};
pub use sevenzip::{SevenZipBackend, SevenZipLocator};
pub use types::*;
pub use unrar::{UnrarBackend, UnrarLocator};

#[cfg(all(test, unix))]
mod process_contract_tests;
