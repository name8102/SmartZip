#!/usr/bin/env python3
"""Generate embedded detection integration test fixtures.

Creates binary files that exercise boundary cases for archive detection:
  - ZIP disguised as JPEG
  - JPEG header prepended to a ZIP payload
  - Low-ratio embedded ZIP in large file
  - ZIP with no extension
  - ZIP containing fake docx structure
  - .docx that is actually a plain ZIP
  - ZIP containing fake CBZ structure
  - Two ZIPs in one file, one dominant

Usage:
    python3 tests/fixtures/embedded/generate.py
"""

import os
import struct
import zipfile
import zlib
from pathlib import Path

FIXTURES_DIR = Path(__file__).parent.resolve()
OUT = FIXTURES_DIR  # write fixtures next to this script


def _minimal_zip(entries: list[tuple[str, bytes]]) -> bytes:
    """Build a minimal valid ZIP in memory."""
    buf = bytearray()
    offsets = []
    for name, data in entries:
        offsets.append(len(buf))
        crc = zlib.crc32(data) & 0xFFFFFFFF
        compressed = zlib.compress(data, wbits=-15)
        name_b = name.encode("utf-8")
        # local file header
        buf += b"PK\x03\x04"
        buf += struct.pack("<HHHHHIIIHH",
            20, 0, 8, 0, 0, crc, len(compressed), len(data), len(name_b), 0)
        buf += name_b
        buf += compressed
    cd_start = len(buf)
    for i, (name, data) in enumerate(entries):
        crc = zlib.crc32(data) & 0xFFFFFFFF
        compressed = zlib.compress(data, wbits=-15)
        name_b = name.encode("utf-8")
        buf += b"PK\x01\x02"
        buf += struct.pack("<HHHHHHIIIHHHHHII",
            20, 20, 0, 8, 0, 0, crc, len(compressed), len(data),
            len(name_b), 0, 0, 0, 0, 0, offsets[i])
        buf += name_b
    cd_size = len(buf) - cd_start
    buf += b"PK\x05\x06"
    buf += struct.pack("<HHHHIIH", 0, 0, len(entries), len(entries), cd_size, cd_start, 0)
    return bytes(buf)


def _jpeg_header() -> bytes:
    """Minimal JPEG header (SOI + APP0 marker)."""
    return b"\xff\xd8\xff\xe0" + b"\x00" * 12  # JFIF marker + padding


def gen_direct_zip_renamed_jpg():
    """ZIP at offset 0 with .jpg extension."""
    data = _minimal_zip([("hello.txt", b"Hello from disguised ZIP!\n")])
    (OUT / "direct_zip_renamed_jpg.jpg").write_bytes(data)
    print("  direct_zip_renamed_jpg.jpg")


def gen_jpg_prefix_rar_dominant():
    """JPEG header prepended to a ZIP payload (carrier scenario)."""
    header = _jpeg_header()  # 16 bytes
    zip_payload = b"Hidden payload content! " * 200  # ~5KB content
    zip_data = _minimal_zip([("secret.txt", zip_payload)])
    (OUT / "jpg_prefix_rar_dominant.jpg").write_bytes(header + zip_data)
    print("  jpg_prefix_rar_dominant.jpg")


def gen_root_embedded_zip_low_ratio():
    """Small ZIP embedded in a large file (< 10% ratio)."""
    zip_data = _minimal_zip([("tiny.txt", b"Small!\n")])
    padding = b"\x42" * (10 * 1024 * 1024)  # 10 MB of 'B'
    (OUT / "root_embedded_zip_low_ratio.bin").write_bytes(padding + zip_data)
    print("  root_embedded_zip_low_ratio.bin")


def gen_nested_no_extension_zip():
    """ZIP with no extension, nested inside an outer ZIP."""
    inner = _minimal_zip([("data.txt", b"No extension data!\n")])
    # wrap in outer zip
    outer = _minimal_zip([("nested_no_extension", inner)])
    (OUT / "nested_no_extension_zip").write_bytes(outer)
    print("  nested_no_extension_zip")


def gen_nested_docx_business_container():
    """ZIP whose entry paths mimic a real .docx (business container)."""
    entries = [
        ("[Content_Types].xml", b'<?xml version="1.0"?><Types/>'),
        ("word/document.xml", b'<?xml version="1.0"?><document/>'),
        ("word/styles.xml", b'<?xml version="1.0"?><styles/>'),
    ]
    data = _minimal_zip(entries)
    (OUT / "nested_docx_business_container.zip").write_bytes(data)
    print("  nested_docx_business_container.zip")


def gen_nested_fake_docx_real_zip():
    """A .docx file that contains only plain text entries (not real docx)."""
    entries = [
        ("readme.txt", b"This is not a real docx.\n"),
        ("notes.txt", b"Just plain text inside.\n"),
    ]
    data = _minimal_zip(entries)
    (OUT / "nested_fake_docx_real_zip.docx").write_bytes(data)
    print("  nested_fake_docx_real_zip.docx")


def gen_nested_cbz_should_skip():
    """ZIP whose entries look like a CBZ (comic book archive)."""
    # CBZ: majority of entries are images
    entries = [
        ("page001.jpg", b"\xff\xd8\xff" + b"\x00" * 50),
        ("page002.jpg", b"\xff\xd8\xff" + b"\x00" * 50),
        ("page003.png", b"\x89PNG" + b"\x00" * 50),
        ("cover.webp", b"RIFF" + b"\x00" * 50),
    ]
    data = _minimal_zip(entries)
    (OUT / "nested_cbz_should_skip.zip").write_bytes(data)
    print("  nested_cbz_should_skip.zip")


def gen_multi_payload_largest_80():
    """Two ZIP payloads concatenated, one dominant (~80%)."""
    small = _minimal_zip([("a.txt", b"A\n")])
    large_content = b"Large B payload\n" * 5000
    large = _minimal_zip([("b.txt", large_content)])
    # Pad so large is ~80% of total: total = pad + small + large
    # ratio = large / (pad + small + large) ≈ 0.80
    total_target = int(len(large) / 0.80)
    pad_size = max(0, total_target - len(small) - len(large))
    padding = b"\x00" * pad_size
    (OUT / "multi_payload_largest_80.bin").write_bytes(padding + small + large)
    print("  multi_payload_largest_80.bin")


def main():
    print("Generating embedded detection fixtures...")
    gen_direct_zip_renamed_jpg()
    gen_jpg_prefix_rar_dominant()
    gen_root_embedded_zip_low_ratio()
    gen_nested_no_extension_zip()
    gen_nested_docx_business_container()
    gen_nested_fake_docx_real_zip()
    gen_nested_cbz_should_skip()
    gen_multi_payload_largest_80()
    print("Done.")


if __name__ == "__main__":
    main()
