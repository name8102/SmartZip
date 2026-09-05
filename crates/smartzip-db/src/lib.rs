//! SQLite persistence layer for SmartZip.

pub mod file_extractions;
pub mod known_files;
pub mod password;
pub mod sample_hash;
pub mod schema;
pub mod task;
pub mod task_event;
pub mod timestamp;

use rusqlite::Connection;
use std::path::Path;

pub type Result<T> = std::result::Result<T, DbError>;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Migration(#[from] rusqlite_migration::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub struct SmartZipDb {
    conn: Connection,
    path: Option<std::path::PathBuf>,
}

impl SmartZipDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path.as_ref())?;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        let mut conn = Connection::open(path.as_ref())?;
        schema::migrate(&mut conn)?;
        Ok(Self {
            conn,
            path: Some(path.as_ref().to_path_buf()),
        })
    }

    pub fn in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        schema::migrate(&mut conn)?;
        Ok(Self { conn, path: None })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Returns the file path if this is a persistent (on-disk) database,
    /// or `None` if it is an in-memory database.
    pub fn db_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}
