from contextlib import closing
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

from migrate_database import migrate_database


class MigrationTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.source = self.root / "legacy" / "smartzip.db"
        self.target = self.root / "current" / "smartzip.db"
        self.source.parent.mkdir()

    def create_source(self):
        connection = sqlite3.connect(self.source)
        connection.execute("CREATE TABLE records (value TEXT)")
        connection.execute("INSERT INTO records VALUES ('retained')")
        connection.execute("PRAGMA user_version = 42")
        connection.commit()
        return connection

    def test_committed_wal_is_migrated_without_sidecars_and_source_is_retained(self):
        original = self.create_source()
        self.addCleanup(original.close)
        original.execute("PRAGMA journal_mode = WAL")
        original.execute("PRAGMA wal_autocheckpoint = 0")
        original.execute("INSERT INTO records VALUES ('in WAL')")
        original.commit()
        self.assertGreater(Path(f"{self.source}-wal").stat().st_size, 0)
        migrate_database(self.source, self.target)
        with closing(sqlite3.connect(self.target)) as migrated:
            self.assertEqual(migrated.execute("SELECT value FROM records").fetchall(),
                             [("retained",), ("in WAL",)])
            self.assertEqual(migrated.execute("PRAGMA user_version").fetchone(), (42,))
        self.assertTrue(self.source.exists())
        self.assertFalse(Path(f"{self.target}-wal").exists())
        self.assertFalse(Path(f"{self.target}-shm").exists())
        if os.name == "posix":
            self.assertEqual(self.target.stat().st_mode & 0o777, 0o600)

    def test_existing_destination_or_sidecars_are_never_overwritten(self):
        self.create_source().close()
        self.target.parent.mkdir()
        for suffix in ("", "-wal", "-shm"):
            with self.subTest(suffix=suffix):
                path = Path(f"{self.target}{suffix}")
                path.write_bytes(b"existing")
                with self.assertRaises(FileExistsError):
                    migrate_database(self.source, self.target)
                self.assertEqual(path.read_bytes(), b"existing")
                path.unlink()

    def test_publish_race_does_not_overwrite_destination_or_lose_source(self):
        self.create_source().close()
        before = self.source.read_bytes()

        def concurrent_create(*_):
            self.target.write_bytes(b"another database")
            raise FileExistsError("destination created concurrently")

        with patch("migrate_database.os.link", side_effect=concurrent_create):
            with self.assertRaises(FileExistsError):
                migrate_database(self.source, self.target)
        self.assertEqual(self.source.read_bytes(), before)
        self.assertEqual(self.target.read_bytes(), b"another database")
        self.assertEqual(list(self.target.parent.glob(".smartzip-migration-*")), [])

    def test_invalid_source_leaves_no_destination_or_temporary_database(self):
        self.source.write_bytes(b"not a SQLite database")
        with self.assertRaises(sqlite3.DatabaseError):
            migrate_database(self.source, self.target)
        self.assertEqual(self.source.read_bytes(), b"not a SQLite database")
        self.assertFalse(self.target.exists())
        self.assertEqual(list(self.target.parent.iterdir()), [])

    def test_missing_source_is_not_created(self):
        with self.assertRaises(FileNotFoundError):
            migrate_database(self.source, self.target)
        self.assertFalse(self.source.exists())
        self.assertFalse(self.target.exists())


if __name__ == "__main__":
    unittest.main()
