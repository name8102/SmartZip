#!/usr/bin/env python3
"""Required beta acceptance with real 7-Zip and deterministic process faults.
No missing-backend skips. All artifacts and databases live in a temporary tree.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import select
import pty
import termios
import shutil
import signal
import sqlite3
import stat
import subprocess
import sys
import tempfile
import time
import zipfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    seven = shutil.which("7z") or shutil.which("7zz")
    if not seven:
        raise SystemExit("REQUIRED: install 7z or 7zz; release tests cannot skip backend coverage")
    checks = []
    with tempfile.TemporaryDirectory(prefix="smartzip-beta-") as workspace:
        root = Path(workspace)
        db = root / "history.db"
        config = root / "backend.toml"
        config.write_text('[backends]\nauto_discover = false\n[[backends.installations]]\nid = "required-7z"\nfamily = "seven-zip-cli"\nexecutable = ' + json.dumps(seven) + '\n')
        env = dict(os.environ, XDG_DATA_HOME=str(root / "data"), XDG_CONFIG_HOME=str(root / "config"))
        base = [str(binary), "--config", str(config), "--db", str(db)]

        def run(arguments, expected=0, command=None):
            result = subprocess.run((command or base) + list(map(str, arguments)), stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True, env=env, timeout=30)
            assert result.returncode == expected, (arguments, result.returncode, result.stdout, result.stderr)
            report = json.loads(result.stdout)
            if isinstance(report, dict) and "exit_code" in report:
                assert report["exit_code"] == result.returncode, report
            if isinstance(report, dict) and "task_id" in report and "status" in report:
                with sqlite3.connect(db) as conn:
                    row = conn.execute("SELECT status FROM tasks WHERE id=?", (report["task_id"],)).fetchone()
                assert row and row[0] == report["status"], (row, report)
            return report

        doctor = run(["doctor", "--json"])
        assert doctor["backends"][0]["version"]
        checks.append("doctor: real backend version and capabilities")
        archive = root / "ordinary.zip"
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as z:
            z.writestr("one.txt", "hello")
            z.writestr("sub/two.txt", "world")
        original_hash = hashlib.sha256(archive.read_bytes()).hexdigest()
        for dest in [root / "first", root / "second"]:
            report = run(["extract", archive, "--output", dest, "--layout", "raw", "--json"])
            assert report["processed_count"] == 1
            assert (dest / "ordinary" / "one.txt").read_text() == "hello"
        checks.append("real ZIP extraction and repeated input to a new destination")
        report = run(["extract", archive, archive, "--output", root / "duplicate", "--json"])
        assert report["status"] == "completed" and report["processed_count"] == 1 and report["skipped_count"] == 1
        report = run(["extract", archive, "--output", root / "first", "--layout", "raw", "--json"])
        assert report["status"] == "completed" and report["skipped_count"] == 1
        checks.append("benign duplicate and collision skip remain successful")
        report = run(["extract", root / "missing.zip", archive, "--output", root / "mixed", "--json"], 2)
        assert report["failed_count"] == 1 and report["processed_count"] == 1
        checks.append("mixed batch, JSON, history and exit status agree")
        run(["detect", root / "missing.zip", "--json"], 1)
        run(["encoding-preview", root / "missing.zip", "--json"], 1)
        checks.append("unreadable detect and all-failed encoding preview return failure")
        ambiguous = root / "ambiguous.zip"
        with zipfile.ZipFile(ambiguous, "w") as z:
            z.writestr("bad.txt", "hello")
        ambiguous.write_bytes(ambiguous.read_bytes().replace(b"bad.txt", b"\xff\xff\xff.txt"))
        report = run(["extract", ambiguous, archive, "--output", root / "encoding-batch", "--json"])
        assert report["processed_count"] == 1 and report["skipped_count"] == 1 and report["failed_count"] == 0
        checks.append("suspicious encoding skip continues the remaining batch")
        for limit in [["--max-output-bytes", "1"], ["--max-files", "1"], ["--min-free-bytes", str(2**63)]]:
            report = run(["extract", archive, "--output", root / "first", "--layout", "raw", "--on-conflict", "overwrite", "--json"] + limit, 1)
            assert (root / "first/ordinary/one.txt").read_text() == "hello"
            assert not list((root / "first").glob(".smartzip-*"))
        checks.append("file/byte/free-space limits roll back staging and retain old output")
        for name, entry in [("traversal", "../escaped.txt"), ("absolute", str(root / "escaped.txt"))]:
            bad = root / f"{name}.zip"
            with zipfile.ZipFile(bad, "w") as z:
                z.writestr(entry, "do not write")
            run(["extract", bad, "--output", root / name, "--json"], 1)
            assert not (root / "escaped.txt").exists()
        link_archive = root / "link.zip"
        with zipfile.ZipFile(link_archive, "w") as z:
            info = zipfile.ZipInfo("link")
            info.create_system = 3
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
            z.writestr(info, str(root))
            z.writestr("link/escaped.txt", "do not write")
        run(["extract", link_archive, "--output", root / "links", "--json"], 1)
        assert not (root / "escaped.txt").exists()
        checks.append("dangerous paths and link traversal rejected before commit")
        payload = root / "payload.bin"
        payload.write_bytes(os.urandom(128 * 1024))
        protected = root / "protected.7z"
        subprocess.run([seven, "a", "-psecret", "-mhe=on", str(protected), str(payload)], check=True, stdout=subprocess.DEVNULL)
        run(["test", protected, "-psecret", "--json"])
        run(["extract", protected, "-psecret", "--output", root / "protected-out", "--json"])
        run(["test", protected, "-pwrong", "--password-limit", "0", "--json"], 1)
        run(["extract", protected, "--password-limit", "0", "--output", root / "no-password", "--json"], 1)
        checks.append("real encrypted 7z: correct, wrong and unavailable passwords")
        # Real terminal: echo disabled, whitespace preserved, Ctrl+C restores it.
        whitespace = root / "whitespace.7z"
        secret = " space secret "
        subprocess.run([seven, "a", "-p" + secret, "-mhe=on", str(whitespace), str(payload)], check=True, stdout=subprocess.DEVNULL)
        for cancel in [False, True]:
            master, slave = pty.openpty()
            process = subprocess.Popen([str(binary), "--config", str(config), "--db", str(root / f"pty-{cancel}.db")] + ["extract", str(whitespace), "--password-limit", "0", "--output", str(root / ("pty-cancel" if cancel else "pty-output"))],
                                       stdin=slave, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
            deadline = time.monotonic() + 10
            try:
                while termios.tcgetattr(slave)[3] & termios.ECHO:
                    assert process.poll() is None and time.monotonic() < deadline, "hidden prompt did not start"
                    time.sleep(.02)
                if cancel:
                    process.send_signal(signal.SIGINT)
                else:
                    os.write(master, (secret + "\n").encode())
                out, err = process.communicate(timeout=5)
                assert process.returncode == (130 if cancel else 0), (out, err)
                assert termios.tcgetattr(slave)[3] & termios.ECHO, "terminal echo was not restored"
                ready, _, _ = select.select([master], [], [], 0)
                assert not ready or secret.encode() not in os.read(master, 65536), "password was echoed"
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
                os.close(master)
                os.close(slave)
        assert next((root / "pty-output").rglob("payload.bin")).read_bytes() == payload.read_bytes()
        checks.append("terminal password preserves whitespace, hides echo, cancels and restores terminal")
        split = root / "split.7z"
        subprocess.run([seven, "a", "-mx0", "-v32k", str(split), str(payload)], check=True, stdout=subprocess.DEVNULL)
        run(["test", str(split) + ".001", "--json"])
        Path(str(split) + ".002").unlink()
        run(["test", str(split) + ".001", "--json"], 1)
        corrupt = root / "corrupt.zip"
        corrupt.write_bytes(b"PK\x03\x04broken")
        run(["test", corrupt, "--json"], 1)
        checks.append("real split 7z, missing volume and corrupted ZIP")
        # Fault process runs through the actual SevenZipBackend subprocess and
        # router staging code, so cancellation is deterministic on both targets.
        fake = root / "slow7z"
        marker = root / "started"
        fake.write_text(f'''#!{sys.executable}
import os, pathlib, sys, time
if sys.argv[1] == 'i': print('7-Zip 26.03'); sys.exit(0)
if sys.argv[1] == 't': print('Everything is Ok'); sys.exit(0)
if sys.argv[1] == 'l': print('Path = file.bin\\nSize = 0'); sys.exit(0)
output = pathlib.Path(next(x[2:] for x in sys.argv if x.startswith('-o')))
output.mkdir(parents=True, exist_ok=True)
pathlib.Path({str(marker)!r}).write_text(str(os.getpid()))
with (output / 'growing.bin').open('wb', buffering=0) as f:
    while True: f.write(bytes(4096)); time.sleep(.02)
''')
        fake.chmod(0o755)
        fake_config = root / "fake.toml"
        fake_config.write_text('[backends]\nauto_discover = false\n[[backends.installations]]\nid = "fault-process"\nfamily = "seven-zip-cli"\nexecutable = ' + json.dumps(str(fake)) + '\n')
        fake_base = [str(binary), "--config", str(fake_config), "--db", str(db)]
        report = run(["extract", archive, "--output", root / "dynamic", "--json", "--max-output-bytes", "16384"], 1, fake_base)
        assert report["status"] == "failed" and not list((root / "dynamic").glob(".smartzip-*"))
        marker.unlink()
        process = subprocess.Popen(fake_base + ["extract", str(archive), "--output", str(root / "cancelled"), "--json"],
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
        limit = time.monotonic() + 10
        while not marker.exists():
            assert process.poll() is None and time.monotonic() < limit, "backend did not start"
            time.sleep(.02)
        child_pid = int(marker.read_text())
        process.send_signal(signal.SIGINT)
        out, err = process.communicate(timeout=5)
        report = json.loads(out)
        assert process.returncode == 130 and report["status"] == "cancelled", (out, err)
        assert not list((root / "cancelled").glob(".smartzip-*"))
        try:
            os.kill(child_pid, 0)
        except ProcessLookupError:
            pass
        else:
            raise AssertionError("cancelled backend is still alive")
        with sqlite3.connect(db) as conn:
            assert conn.execute("SELECT status FROM tasks WHERE id=?", (report["task_id"],)).fetchone()[0] == "cancelled"
        checks.append("growing external output: dynamic stop; Ctrl+C reaps child and cleans staging")
        # Unicode truncation exercises a former byte-boundary panic.
        subprocess.run(base + ["password", "add", "中文🙂" * 15], check=True, capture_output=True, env=env)
        listed = subprocess.run(base + ["password", "list"], check=True, capture_output=True, text=True, env=env)
        assert "中文" in listed.stdout
        assert stat.S_IMODE(db.stat().st_mode) == 0o600
        assert hashlib.sha256(archive.read_bytes()).hexdigest() == original_hash
        checks.append("Unicode password listing, private database mode and source preservation")
    print(json.dumps({"passed": len(checks), "checks": checks}, indent=2))


if __name__ == "__main__":
    main()
