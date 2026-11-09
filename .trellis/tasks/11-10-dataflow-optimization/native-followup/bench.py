"""Run with SMARTZIP_7Z_LIBRARY set. Each CLI must have its matching worker beside it."""
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import warnings
import zipfile


def make_inputs(root):
    for count in (5000, 50000):
        with zipfile.ZipFile(root / f"{count}.zip", "w") as archive:
            for i in range(count):
                archive.writestr(f"dir/{i:05d}.txt", b"x")
    with zipfile.ZipFile(root / "large.zip", "w") as archive:
        with archive.open("data.bin", "w") as member:
            chunk = bytes(1024 * 1024)
            for _ in range(128):
                member.write(chunk)


def cli(binary, args):
    env = {**os.environ, "SMARTZIP_ARCHIVE_WORKER": str(binary.with_name("smartzip-archive-worker"))}
    subprocess.run(
        [binary, "--no-config", "--stateless", "--non-interactive", "--no-recursive", *args],
        env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, check=True,
    )


def read_index(binary, path):
    library = os.environ["SMARTZIP_7Z_LIBRARY"]
    request = json.dumps({
        "library": list(os.fsencode(library)),
        "input": {"path": list(os.fsencode(path)), "offset": 0, "length": None, "volumes": []},
        "name": path.name, "format": "zip", "password": None,
        "operation": {"Decode": {"indices": [49999]}},
    }).encode()
    result = subprocess.run(
        [binary.with_name("smartzip-archive-worker")],
        input=struct.pack("<Q", len(request)) + request,
        capture_output=True, check=True,
    )
    return result.stdout


def check_member(data):
    payload = bytearray()
    offset = 0
    while offset < len(data):
        frame = data[offset]
        offset += 1
        if frame == 0:
            size = struct.unpack_from("<Q", data, offset)[0]
            event = json.loads(data[offset + 8 : offset + 8 + size])
            assert "Error" not in event, event
            offset += 8 + size
        elif frame == 1:
            index, size = struct.unpack_from("<II", data, offset)
            assert index == 49999
            payload.extend(data[offset + 8 : offset + 8 + size])
            offset += 8 + size
        elif frame == 2:
            offset += 4
        else:
            raise AssertionError(f"unknown frame {frame}")
    assert payload == b"x"


def main(before, after, selected=None):
    with tempfile.TemporaryDirectory(prefix="smartzip-native-flow-") as tmp:
        root = Path(tmp)
        make_inputs(root)
        many = str(root / "50000.zip")
        with warnings.catch_warnings():
            warnings.filterwarnings("ignore", message="Duplicate name:")
            with zipfile.ZipFile(root / "duplicates.zip", "w") as archive:
                for i in range(2000):
                    archive.writestr("file.txt", str(i).encode())
        cases = {
            "detect_50k": ["detect", many],
            "list_50k": ["list", many],
            "test_50k": ["test", "--diagnose", "off", many],
            "extract_128MiB": ["extract", "--layout", "raw", "-o", str(root / "out"), str(root / "large.zip")],
            "extract_duplicates_2k": ["extract", "--layout", "raw", "-o", str(root / "out"), str(root / "duplicates.zip")],
            "enc_5k": ["enc", str(root / "5000.zip")],
            "worker_read_one_of_50k": None,
        }
        for case, args in cases.items():
            if selected is not None and case != selected:
                continue
            samples = {"before": [], "after": []}
            wire_bytes = {}
            for _ in range(6):
                for label, binary in [("before", before), ("after", after)]:
                    start = time.perf_counter()
                    if args is None:
                        data = read_index(binary, root / "50000.zip")
                    else:
                        cli(binary, args)
                    samples[label].append(round(time.perf_counter() - start, 4))
                    if args is None:
                        check_member(data)
                        wire_bytes[label] = len(data)
                    if case == "extract_duplicates_2k":
                        assert len(list((root / "out" / "duplicates").iterdir())) == 2000
                    if case.startswith("extract_"):
                        shutil.rmtree(root / "out")
            row = {"case": case, "seconds": {k: v[1:] for k, v in samples.items()},
                   "warmup_s": {k: v[0] for k, v in samples.items()}}
            if wire_bytes:
                row["wire_bytes"] = wire_bytes
            print(json.dumps(row), flush=True)


if __name__ == "__main__":
    main(*(Path(arg).resolve() for arg in sys.argv[1:3]), *sys.argv[3:])
