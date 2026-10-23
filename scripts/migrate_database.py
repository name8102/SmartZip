#!/usr/bin/env python3
"""One-time SQLite path migration; stop all SmartZip processes before running.

The source database (including its WAL/SHM) is retained at its original path for
rollback. SmartZip uses only the new path after this migration. Do not run an old
SmartZip version against the retained source after migrating.
"""

import argparse
from contextlib import closing
import os
from pathlib import Path
import sqlite3
import tempfile


def migrate_database(source: Path, destination: Path) -> None:
    source = source.resolve(strict=True)
    destination = destination.absolute()
    if not source.is_file():
        raise ValueError(f"source is not a database file: {source}")
    # Never attach a stale WAL to the newly published snapshot, nor replace an
    # existing database, directory or dangling symlink.
    for path in (destination, Path(f"{destination}-wal"), Path(f"{destination}-shm")):
        if os.path.lexists(path):
            raise FileExistsError(f"destination already exists: {path}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".smartzip-migration-", dir=destination.parent)
    os.close(fd)  # mkstemp creates a private (0600) database file.
    try:
        # SQLite's backup API reads committed WAL data into one self-contained
        # database. Copying/renaming the db, WAL and SHM independently is unsafe.
        with closing(sqlite3.connect(source.as_uri() + "?mode=ro", uri=True)) as original:
            with closing(sqlite3.connect(temporary)) as snapshot:
                original.backup(snapshot)
                snapshot.execute("PRAGMA journal_mode = DELETE")
                if snapshot.execute("PRAGMA quick_check").fetchall() != [("ok",)]:
                    raise sqlite3.DatabaseError("migrated database failed quick_check")
        with open(temporary, "rb") as database:
            os.fsync(database.fileno())
        # Same-directory hard link publishes atomically without overwriting even
        # if another process created the destination after our initial check.
        os.link(temporary, destination)
        if os.name == "posix":
            directory = os.open(destination.parent, os.O_RDONLY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
    finally:
        os.unlink(temporary)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    migrate_database(args.source, args.destination)
    print(f"Database migrated to {args.destination}; source retained at {args.source}")


if __name__ == "__main__":
    main()
