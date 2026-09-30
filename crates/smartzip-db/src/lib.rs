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
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, DbError>;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("another SmartZip execution session owns {0}")]
    OwnerBusy(std::path::PathBuf),
    #[error("hard-linked databases are unsupported; use one database path: {0}")]
    HardLinkedDatabase(PathBuf),
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
        let path = database_path(path.as_ref(), true)?;
        let lock_path = owner_lock_path(&path);
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
        let mut db = Self::open_resolved(&path)?;
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
        let path = database_path(path.as_ref(), true)?;
        Self::open_resolved(&path)
    }

    fn open_resolved(path: &Path) -> Result<Self> {
        // Do not open/close a separate database FD on Unix: closing any FD for
        // its inode can release another SQLite connection's POSIX locks.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if path.try_exists()? {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            }
        }
        let mut conn = Connection::open(path)?;
        check_database_file(path)?;
        if std::fs::canonicalize(path)? != path {
            return Err(std::io::Error::other("database path changed while opening").into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // A newly created SQLite file is still empty; make it private before
            // migration or any caller can populate sensitive rows or WAL data.
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        schema::migrate(&mut conn)?;
        Ok(Self {
            conn,
            path: Some(path.to_path_buf()),
        })
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = database_path(path.as_ref(), false)?;
        let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self {
            conn,
            path: Some(path),
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

/// Resolve aliases before selecting either the execution lock or SQLite sidecars.
/// Hard links are rejected instead of moved/unlinked: SQLite WAL/SHM identity is
/// path-based, so opening a second hard-link name is unsafe even with one owner.
fn database_path(path: &Path, create: bool) -> Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(path) => {
            check_database_file(&path)?;
            Ok(path)
        }
        Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
            // A dangling final symlink must not turn into a different database.
            if std::fs::symlink_metadata(path).is_ok() {
                return Err(error.into());
            }
            let absolute = std::path::absolute(path)?;
            let parent = absolute.parent().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "database needs a file name",
                )
            })?;
            std::fs::create_dir_all(parent)?;
            Ok(
                std::fs::canonicalize(parent)?.join(absolute.file_name().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "database needs a file name",
                    )
                })?),
            )
        }
        Err(error) => Err(error.into()),
    }
}

fn check_database_file(path: &Path) -> Result<()> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "database path must be a regular file",
        )
        .into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() > 1 {
            return Err(DbError::HardLinkedDatabase(path.into()));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        let file = std::fs::File::open(path)?;
        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // The live File supplies a valid handle and the API initializes info on success.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if unsafe { info.assume_init() }.nNumberOfLinks > 1 {
            return Err(DbError::HardLinkedDatabase(path.into()));
        }
    }
    Ok(())
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

    #[test]
    fn missing_database_parents_are_created_only_for_writes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("nested/state/state.db");
        assert!(SmartZipDb::open_read_only(&path).is_err());
        assert!(!path.parent().unwrap().exists());
        let owner = SmartZipDb::acquire_execution(&path).unwrap();
        assert_eq!(
            owner.database().db_path(),
            Some(path.canonicalize().unwrap().as_path())
        );
        assert_eq!(owner.epoch(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliases_share_owner_and_sqlite_wal_path() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let alias = root.path().join("alias.db");
        let first = SmartZipDb::acquire_execution(&path).unwrap();
        first
            .database()
            .connection()
            .execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE alias_probe(value TEXT); \
             INSERT INTO alias_probe VALUES ('synthetic');",
            )
            .unwrap();
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert!(matches!(
            SmartZipDb::acquire_execution(&alias),
            Err(DbError::OwnerBusy(_))
        ));
        for db in [
            SmartZipDb::open(&alias).unwrap(),
            SmartZipDb::open_read_only(&alias).unwrap(),
        ] {
            assert_eq!(db.db_path(), first.database().db_path());
            assert_eq!(
                db.connection()
                    .query_row("SELECT value FROM alias_probe", [], |row| row
                        .get::<_, String>(0))
                    .unwrap(),
                "synthetic"
            );
        }
        assert!(!owner_lock_path(&alias).exists());
        assert!(!root.path().join("alias.db-wal").exists());
        drop(first);
        let second = SmartZipDb::acquire_execution(&alias).unwrap();
        assert_eq!(second.epoch(), 2);
    }

    #[test]
    fn hard_link_aliases_are_rejected_without_touching_owner_or_data() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let alias = root.path().join("alias.db");
        let first = SmartZipDb::acquire_execution(&path).unwrap();
        first
            .database()
            .connection()
            .execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE hard_link_probe(value TEXT); \
             INSERT INTO hard_link_probe VALUES ('synthetic');",
            )
            .unwrap();
        std::fs::hard_link(&path, &alias).unwrap();
        for result in [
            SmartZipDb::open(&alias),
            SmartZipDb::open_read_only(&alias),
            SmartZipDb::open(&path),
        ] {
            assert!(matches!(result, Err(DbError::HardLinkedDatabase(_))));
        }
        assert!(matches!(
            SmartZipDb::acquire_execution(&alias),
            Err(DbError::HardLinkedDatabase(_))
        ));
        assert_eq!(
            first
                .database()
                .connection()
                .query_row("SELECT epoch FROM execution_owner WHERE id=1", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
        assert_eq!(
            first
                .database()
                .connection()
                .query_row("SELECT value FROM hard_link_probe", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "synthetic"
        );
        assert!(!owner_lock_path(&alias).exists());
        assert!(!root.path().join("alias.db-wal").exists());
        drop(first);
        std::fs::remove_file(&alias).unwrap();
        assert_eq!(SmartZipDb::acquire_execution(&path).unwrap().epoch(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_parent_aliases_share_owner_before_database_creation() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("real");
        std::fs::create_dir(&parent).unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&parent, &alias).unwrap();
        let first = SmartZipDb::acquire_execution(parent.join("nested/state.db")).unwrap();
        assert!(matches!(
            SmartZipDb::acquire_execution(alias.join("nested/state.db")),
            Err(DbError::OwnerBusy(_))
        ));
        assert_eq!(first.epoch(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn opening_alias_keeps_existing_sqlite_transaction_locked() {
        const PROBE_PATH: &str = "SMARTZIP_DB_ALIAS_LOCK_PROBE_PATH";
        if let Some(path) = std::env::var_os(PROBE_PATH) {
            let conn = Connection::open(PathBuf::from(path)).unwrap();
            conn.busy_timeout(std::time::Duration::from_millis(100))
                .unwrap();
            let error = conn
                .execute("INSERT INTO lock_probe VALUES (2)", [])
                .unwrap_err();
            assert_eq!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy)
            );
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let alias = root.path().join("alias.db");
        let db = SmartZipDb::open(&path).unwrap();
        db.connection().execute_batch("CREATE TABLE lock_probe(value INTEGER); BEGIN IMMEDIATE; INSERT INTO lock_probe VALUES (1);").unwrap();
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        drop(SmartZipDb::open_read_only(&alias).unwrap());
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "execution_owner_tests::opening_alias_keeps_existing_sqlite_transaction_locked",
            ])
            .env(PROBE_PATH, &path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "writer escaped SQLite lock: {}",
            String::from_utf8_lossy(&result.stdout)
        );
        db.connection().execute_batch("ROLLBACK").unwrap();
    }
}
