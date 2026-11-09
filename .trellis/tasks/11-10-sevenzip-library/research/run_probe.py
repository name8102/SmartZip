#!/usr/bin/env python3
"""Reproduce the Linux API spike. 7zz is used only to generate fixtures."""
import argparse
import gzip
import io
import json
import lzma
from pathlib import Path
import subprocess
import tarfile
import tempfile
import zipfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("probe", type=Path)
    parser.add_argument("library", type=Path)
    parser.add_argument("--fixture-cli", default="7zz")
    parser.add_argument("--plain-only", action="store_true", help="Rust spike omits password callbacks")
    args = parser.parse_args()
    payload = b"native library fixture\n" * 1024
    results = []
    with tempfile.TemporaryDirectory(prefix="smartzip-native-probe-") as directory:
        root = Path(directory)
        name = "中文\nPath = injected.txt"
        with zipfile.ZipFile(root / "plain.zip", "w", compression=zipfile.ZIP_STORED) as archive:
            archive.writestr(name, payload)
            archive.writestr("duplicate.txt", b"first")
            archive.writestr("duplicate.txt", b"second")
        corrupt = bytearray((root / "plain.zip").read_bytes())
        offset = 30 + len(name.encode())
        corrupt[offset + 17] ^= 0x80
        (root / "corrupt.zip").write_bytes(corrupt)
        with tarfile.open(root / "plain.tar", "w") as archive:
            info = tarfile.TarInfo("payload.txt")
            info.size = len(payload)
            archive.addfile(info, io.BytesIO(payload))
        (root / "plain.gz").write_bytes(gzip.compress(payload))
        (root / "plain.xz").write_bytes(lzma.compress(payload))
        (root / "payload.txt").write_bytes(payload)
        for archive, flags in [
            ("plain.7z", []),
            ("encrypted.7z", ["-psecret"]),
            ("headers.7z", ["-psecret", "-mhe=on"]),
        ]:
            subprocess.run([args.fixture_cli, "a", "-bd", *flags, str(root / archive),
                            str(root / "payload.txt")], check=True, capture_output=True)

        def check(label, archive, password="-", mode="test", expected=0, contains=()):
            result = subprocess.run([str(args.probe.resolve()), str(args.library.resolve()),
                                     str(root / archive), password, mode],
                                    capture_output=True, text=True, check=False)
            assert result.returncode == expected, (label, result.returncode, result.stdout, result.stderr)
            for token in contains:
                assert token in result.stdout, (label, token, result.stdout)
            results.append({"case": label, "exit": result.returncode, "stdout": result.stdout})

        for archive in ["plain.zip", "plain.7z", "plain.tar", "plain.gz", "plain.xz"]:
            check(archive, archive, contains=("failures=0",))
        check("exact-index-read", "plain.zip", mode="read", contains=(f"bytes={len(payload)}",))
        if not args.plain_only:
            check("data-password-correct", "encrypted.7z", password="secret")
            check("data-password-wrong", "encrypted.7z", password="wrong", expected=4,
                  contains=("extract_hresult=0x00000000", "failures=1"))
            check("header-password-missing", "headers.7z", expected=3, contains=("password_requested=1",))
            check("header-password-correct", "headers.7z", password="secret")
            check("header-password-wrong", "headers.7z", password="wrong", expected=3)
        check("crc-error", "corrupt.zip", expected=4,
              contains=("operation_result=3", "extract_hresult=0x00000000", "failures=1"))
        check("callback-cancel", "plain.zip", mode="cancel", expected=4,
              contains=("extract_hresult=0x80004004", "bytes=0"))
    print(json.dumps({"cases": results, "passed": len(results)}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
