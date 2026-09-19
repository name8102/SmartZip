//! SQLite persistence layer for SmartZip.

pub mod file_extractions;
pub mod known_files;
pub mod password;
pub mod sample_hash;
pub mod schema;
pub mod task;
pub mod task_event;
pub mod task_execution;
pub mod timestamp;

use rusqlite::Connection;
use std::path::Path;

pub type Result<T> = std::result::Result<T, DbError>;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("another SmartZip execution session owns {0}")]
    OwnerBusy(std::path::PathBuf),
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
    pub fn acquire_execution(path: impl AsRef<Path>) -> Result<ExecutionOwner> {
        let lock_path = owner_lock_path(path.as_ref());
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        if let Err(error) = lock.try_lock() {
            return match error {
                std::fs::TryLockError::WouldBlock => Err(DbError::OwnerBusy(lock_path)),
                std::fs::TryLockError::Error(error) => Err(error.into()),
            };
        }
        let mut db = Self::open(path.as_ref())?;
        let session_id = uuid::Uuid::new_v4().to_string();
        let acquired_at = timestamp::now_utc_iso8601();
        let tx = db.conn.transaction()?;
        let previous: i64 = tx
            .query_row("SELECT epoch FROM execution_owner WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);
        let epoch = previous.saturating_add(1);
        tx.execute(
            "INSERT INTO execution_owner(id, epoch, session_id, acquired_at) VALUES (1, ?1, ?2, ?3) \
             ON CONFLICT(id) DO UPDATE SET epoch=excluded.epoch, session_id=excluded.session_id, acquired_at=excluded.acquired_at",
            rusqlite::params![epoch, session_id, acquired_at],
        )?;
        tx.commit()?;
        Ok(ExecutionOwner {
            db,
            _lock: lock,
            epoch,
            session_id,
        })
    }

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

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let conn =
            Connection::open_with_flags(path.as_ref(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self {
            conn,
            path: Some(path.as_ref().into()),
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

    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Returns the file path if this is a persistent (on-disk) database,
    /// or `None` if it is an in-memory database.
    pub fn db_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

pub struct ExecutionOwner {
    db: SmartZipDb,
    _lock: std::fs::File,
    epoch: i64,
    session_id: String,
}

impl ExecutionOwner {
    pub fn database(&self) -> &SmartZipDb {
        &self.db
    }

    pub fn database_mut(&mut self) -> &mut SmartZipDb {
        &mut self.db
    }

    pub fn epoch(&self) -> i64 {
        self.epoch
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

fn owner_lock_path(path: &Path) -> std::path::PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(".owner.lock");
    value.into()
}

#[cfg(test)]
mod execution_owner_tests {
    use super::*;

    #[test]
    fn execution_owner_is_exclusive_and_increments_epoch() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let first = SmartZipDb::acquire_execution(&path).unwrap();
        assert_eq!(first.epoch(), 1);
        assert!(matches!(
            SmartZipDb::acquire_execution(&path),
            Err(DbError::OwnerBusy(_))
        ));
        drop(first);
        let second = SmartZipDb::acquire_execution(&path).unwrap();
        assert_eq!(second.epoch(), 2);
    }
}
