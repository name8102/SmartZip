//! Explicit plaintext reveal: write a private complete file, never replace a target.

use smartzip_db::password::PasswordRepository;
use std::{
    io::{self, Write},
    path::Path,
};

/// Export enabled passwords in ranking order. Existing files, directories and
/// symlinks are preserved. A failed write removes the private sibling temporary.
/// Errors contain no password values, including values that cannot be exported.
pub fn export_passwords(repo: &PasswordRepository<'_>, path: &Path) -> io::Result<usize> {
    let rows = repo
        .ranked_candidates(i64::MAX as usize)
        .map_err(|_| io::Error::other("cannot read password database"))?;
    if rows.iter().any(|row| row.value.contains(['\n', '\r'])) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "passwords containing line breaks cannot be exported as line-based text",
        ));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| io::Error::new(e.kind(), "cannot create private password export"))?;
    for row in &rows {
        writeln!(file, "{}", row.value)
            .map_err(|e| io::Error::new(e.kind(), "cannot write password export"))?;
    }
    file.as_file()
        .sync_all()
        .map_err(|e| io::Error::new(e.kind(), "cannot sync password export"))?;
    file.persist_noclobber(path).map_err(|e| {
        io::Error::new(e.error.kind(),
        "cannot publish password export: destination exists or is not writable; choose a new file")
    })?;
    Ok(rows.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartzip_db::{password::NewPassword, SmartZipDb};

    #[test]
    fn export_is_complete_private_and_never_overwrites() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = PasswordRepository::new(db.connection());
        repo.upsert(NewPassword {
            value: "synthetic-first",
            source: "test",
            pinned: true,
        })
        .unwrap();
        repo.upsert(NewPassword {
            value: "synthetic-second",
            source: "test",
            pinned: false,
        })
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("passwords.txt");
        assert_eq!(export_passwords(&repo, &output).unwrap(), 2);
        assert_eq!(
            std::fs::read_to_string(&output).unwrap(),
            "synthetic-first\nsynthetic-second\n"
        );
        assert_eq!(
            export_passwords(&repo, &output).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            std::fs::read_to_string(&output).unwrap(),
            "synthetic-first\nsynthetic-second\n"
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn invalid_passwords_leave_no_file_or_values_in_errors() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = PasswordRepository::new(db.connection());
        repo.upsert(NewPassword {
            value: "synthetic\nsecret",
            source: "test",
            pinned: false,
        })
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let error = export_passwords(&repo, &root.path().join("passwords.txt")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!format!("{error:?}").contains("synthetic"));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn export_preserves_existing_and_dangling_symlinks() {
        let db = SmartZipDb::in_memory().unwrap();
        let repo = PasswordRepository::new(db.connection());
        repo.upsert(NewPassword {
            value: "synthetic",
            source: "test",
            pinned: false,
        })
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target.txt");
        let output = root.path().join("passwords.txt");
        std::os::unix::fs::symlink(&target, &output).unwrap();
        assert!(export_passwords(&repo, &output).is_err());
        assert!(!target.exists());
        std::fs::write(&target, "preserve").unwrap();
        assert!(export_passwords(&repo, &output).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "preserve");
        assert!(std::fs::symlink_metadata(&output)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }
}
