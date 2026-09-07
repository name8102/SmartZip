#!/usr/bin/env python3
"""Required beta acceptance with real 7-Zip and deterministic process faults.
No missing-backend skips. All artifacts and databases live in a temporary tree.
"""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import select
import pty
import termios
import shutil
import signal
import shlex
import sqlite3
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile
import zlib


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

        def run(arguments, expected=0, command=None, history_db=None):
            result = subprocess.run((command or base) + list(map(str, arguments)), stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True, env=env, timeout=30)
            assert result.returncode == expected, (arguments, result.returncode, result.stdout, result.stderr)
            report = json.loads(result.stdout)
            if isinstance(report, dict) and "exit_code" in report:
                assert report["exit_code"] == result.returncode, report
            if isinstance(report, dict) and "task_id" in report and "status" in report:
                with sqlite3.connect(history_db or db) as conn:
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
        disguised = root / "ordinary.jpg"
        disguised.write_bytes(archive.read_bytes())
        report = run(["extract", disguised, "--output", root / "disguised", "--layout", "raw", "--json"])
        assert report["processed_count"] == 1 and report["skipped_count"] == 0
        assert (root / "disguised/ordinary/one.txt").read_text() == "hello"
        tar_source = root / "tar-source"
        tar_source.mkdir()
        (tar_source / "hello.txt").write_text("hello from tar")
        tar_archive = root / "tar.jpg"
        with tarfile.open(tar_archive, "w", format=tarfile.USTAR_FORMAT) as tar:
            tar.add(tar_source, arcname=".")
        report = run(["extract", tar_archive, "--output", root / "tar-output", "--layout", "raw", "--json"])
        assert report["processed_count"] == 1 and report["skipped_count"] == 0
        assert (root / "tar-output/tar/hello.txt").read_text() == "hello from tar"
        checks.append("ZIP and TAR renamed to JPG; harmless TAR root directory accepted")
        # Explicit roots ignore nested minimum-size/ratio/window gates. Finding
        # a header at offset zero must not hide a later concatenated archive.
        for name, prefix in [("carrier.jpg", b"\xff\xd8\xff" + bytes(100_000)), ("joined.jpg", b"")]:
            joined = root / name
            joined.write_bytes(prefix + archive.read_bytes() + bytes(100) + archive.read_bytes())
            output = root / (name + "-output")
            report = run(["extract", joined, "--max-scan-bytes", "1024", "--recursion-limit", "0", "--output", output, "--layout", "raw", "--json"])
            assert report["processed_count"] == 2 and report["skipped_count"] == 0, report
            stem = joined.stem
            assert sorted(path.name for path in output.iterdir()) == [stem + "-1", stem + "-2"]
            extracted = list(output.rglob("one.txt"))
            assert len(extracted) == 2 and all(p.read_text() == "hello" for p in extracted)
        single_carrier = root / "single-carrier.jpg"
        single_carrier.write_bytes(bytes(1000) + archive.read_bytes())
        report = run(["extract", single_carrier, "--output", root / "single-carrier-output", "--layout", "raw", "--json"])
        assert report["processed_count"] == 1 and (root / "single-carrier-output/single-carrier/one.txt").read_text() == "hello"
        checks.append("small root payloads and concatenated archives at offset zero all extract automatically")
        # The complete archive is larger than the signature window. A later
        # archive must still be found from its actual end, beyond the old cap.
        large_data = root / "large.bin"
        with large_data.open("wb") as f:
            f.truncate(70 * 1024 * 1024)
        large_archive = root / "large.7z"
        subprocess.run([seven, "a", "-mx=0", str(large_archive), str(large_data)], check=True, stdout=subprocess.DEVNULL)
        large_carrier = root / "large.jpg"
        with large_carrier.open("wb") as f:
            f.write(b"\xff\xd8\xff" + bytes(1000))
            with large_archive.open("rb") as source:
                shutil.copyfileobj(source, f)
            f.write(bytes(100))
            f.write(archive.read_bytes())
        report = run(["extract", large_carrier, "--recursion-limit", "0", "--output", root / "large-output", "--layout", "raw", "--json"])
        assert report["processed_count"] == 2 and report["skipped_count"] == 0, report
        assert next((root / "large-output").rglob("large.bin")).stat().st_size == large_data.stat().st_size
        assert next((root / "large-output").rglob("one.txt")).read_text() == "hello"
        checks.append("archive parsing crosses 64 MiB and continues to the following payload")
        document = root / "explicit.docx"
        document.write_bytes(archive.read_bytes())
        report = run(["extract", document, "--output", root / "document-output", "--json"])
        assert report["processed_count"] == 1 and report["skipped_count"] == 0, report
        nested_carrier = root / "nested-carrier.zip"
        with zipfile.ZipFile(nested_carrier, "w", zipfile.ZIP_DEFLATED) as z:
            z.write(root / "carrier.jpg", "carrier.jpg")
        report = run(["extract", nested_carrier, "--output", root / "nested-carrier-output", "--json"])
        assert report["processed_count"] == 1, report
        assert list((root / "nested-carrier-output").rglob("carrier.jpg"))
        checks.append("explicit business-container extension attempts extraction; nested small carriers retain efficiency gates")
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
        no_password_db = root / "no-password.db"
        run(["extract", protected, "--password-limit", "0", "--output", root / "no-password", "--json"], 1,
            [str(binary), "--config", str(config), "--db", str(no_password_db)], no_password_db)
        checks.append("real encrypted 7z: correct, wrong and unavailable passwords")
        disguised_protected = root / "encrypted.jpg"
        disguised_protected.write_bytes(protected.read_bytes())
        report = run(["test", disguised_protected, "-pwrong", "-psecret", "--json"])
        assert report["files"][0]["integrity"] == "intact"
        report = run(["extract", disguised_protected, "-pwrong", "-psecret", "--output", root / "encrypted-jpg", "--json"])
        assert report["processed_count"] == 1 and report["skipped_count"] == 0
        assert next((root / "encrypted-jpg").rglob("payload.bin")).read_bytes() == payload.read_bytes()
        checks.append("encrypted JPG retries remaining passwords after ambiguous encrypted-data error")
        data_encrypted = root / "data-encrypted.7z"
        subprocess.run([seven, "a", "-psecret", "-mhe=off", str(data_encrypted), str(payload)], check=True, stdout=subprocess.DEVNULL)
        trace = root / "password-operations.txt"
        traced_backend = root / "traced-7z"
        traced_backend.write_text("#!/bin/sh\nprintf '%s\\n' \"$1\" >> " + shlex.quote(str(trace)) + "\nexec " + shlex.quote(seven) + " \"$@\"\n")
        traced_backend.chmod(0o700)
        traced_config = root / "traced.toml"
        traced_config.write_text('[backends]\nauto_discover = false\n[[backends.installations]]\nid = "trace"\nfamily = "seven-zip-cli"\nexecutable = ' + json.dumps(str(traced_backend)) + '\n')
        traced_db = root / "traced.db"
        report = run(["extract", data_encrypted, "-pwrong", "-psecret", "--output", root / "traced-output", "--json"],
                     command=[str(binary), "--config", str(traced_config), "--db", str(traced_db)], history_db=traced_db)
        operations = trace.read_text().splitlines()
        assert "t" not in operations and operations.count("x") == 2, operations
        assert report["processed_count"] == 1 and report["failed_count"] == 0
        assert next((root / "traced-output").rglob("payload.bin")).read_bytes() == payload.read_bytes()
        assert not list((root / "traced-output").glob(".smartzip-*"))
        with sqlite3.connect(traced_db) as conn:
            assert conn.execute("SELECT success_count FROM passwords WHERE value='secret'").fetchone()[0] == 1
        checks.append("password attempts extract directly: two x calls, zero full-test calls, failed staging removed")
        exhausted_db = root / "exhausted.db"
        exhausted_base = [str(binary), "--config", str(config), "--db", str(exhausted_db)]
        report = run(["extract", disguised_protected, "-pwrong", "--no-empty", "--password-limit", "0", "--output", root / "exhausted", "--json"], 1, exhausted_base, exhausted_db)
        errors = [event["kind"]["Failed"]["error"] for event in report["events"] if isinstance(event["kind"], dict) and "Failed" in event["kind"]]
        assert any("wrong password or damaged encrypted data" in error for error in errors), report
        with sqlite3.connect(exhausted_db) as conn:
            assert conn.execute("SELECT reason FROM file_extractions WHERE task_id=?", (report["task_id"],)).fetchone()[0] == "password_indeterminate"
            assert conn.execute("SELECT COUNT(*) FROM passwords").fetchone()[0] == 0
        checks.append("exhausted ambiguous passwords retain the diagnostic and history reason without penalties")
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
        # Stored RAR5 volume set, matching the archive diagnostic fixtures.
        def vint(value):
            result = bytearray()
            while value >= 128:
                result.append((value & 127) | 128)
                value >>= 7
            result.append(value)
            return bytes(result)

        def rar_block(body):
            header = vint(len(body)) + body
            return zlib.crc32(header).to_bytes(4, "little") + header

        parts = [b"A" * 100, b"B" * 110, b"C" * 70]
        whole = b"".join(parts)
        nested_volumes = root / "nested-volumes.zip"
        with zipfile.ZipFile(nested_volumes, "w", zipfile.ZIP_DEFLATED) as z:
            for index, part in enumerate(parts):
                last = index == len(parts) - 1
                main = bytes([1, 0, 1 if index == 0 else 3]) + (vint(index) if index else b"")
                file = bytes([2, 2 | (8 if index else 0) | (0 if last else 16)])
                file += vint(len(part)) + b"\x04" + vint(len(whole)) + b"\x00"
                file += zlib.crc32(whole if last else part).to_bytes(4, "little")
                file += bytes([0, 1, 5]) + b"a.bin"
                rar = b"Rar!\x1a\x07\x01\x00" + rar_block(main) + rar_block(file) + part
                rar += rar_block(bytes([5, 0, 0 if last else 1]))
                z.writestr(f"set.part{index + 1:02}.rar", rar)
        report = run(["extract", nested_volumes, "--output", root / "nested-volumes-output", "--layout", "raw", "--json"])
        assert report["failed_count"] == 0 and report["processed_count"] == 2, report
        assert next((root / "nested-volumes-output").rglob("a.bin")).read_bytes() == whole
        checks.append("nested RAR volume set extracts once without retrying consumed members")
        # A recognized volume set is one archive. Signatures in a raw member
        # belong to its compressed/stored payload, not independent root inputs.
        volume_root = root / "disguised-volumes"
        volume_root.mkdir()
        inner = io.BytesIO()
        with zipfile.ZipFile(inner, "w", zipfile.ZIP_STORED) as z:
            z.writestr("inside.txt", b"B" * (11 * 1024 * 1024))
        volume_payload = volume_root / "payload.bin"
        volume_payload.write_bytes(b"A" * (17 * 1024 * 1024) + inner.getvalue() + b"C" * (1024 * 1024))
        subprocess.run([seven, "a", "-mx0", "-v16m", str(volume_root / "parts.7z"), str(volume_payload)], check=True, stdout=subprocess.DEVNULL)
        jpg_volumes = []
        for index, part in enumerate(sorted(volume_root.glob("parts.7z.*")), 1):
            jpg = volume_root / f"part{index:02}.jpg"
            part.rename(jpg)
            jpg_volumes.append(jpg)
        report = run(["extract", jpg_volumes[1], jpg_volumes[0], "--deep", "--embedded", "largest", "--recursion-limit", "0", "--output", root / "volume-output", "--json"])
        assert report["processed_count"] == 1, report
        assert report["processed"][0]["embedded_offset"] is None
        assert next((root / "volume-output").rglob("payload.bin")).read_bytes() == volume_payload.read_bytes()
        checks.append("continuation JPG volume with embedded ZIP bytes keeps the resolved archive identity")
        crc_archive = root / "password.zip"
        with zipfile.ZipFile(crc_archive, "w", zipfile.ZIP_STORED) as z:
            z.writestr("password.txt", "crc-original")
        crc_archive.write_bytes(crc_archive.read_bytes().replace(b"crc-original", b"crc-modified"))
        report = run(["test", crc_archive, "--diagnose", "off", "--json"], 1)
        assert report["files"][0]["integrity"] == "corrupt"
        assert report["files"][0]["password_status"] == "not_needed"
        checks.append("password in archive or entry names cannot change CRC failure classification")
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
