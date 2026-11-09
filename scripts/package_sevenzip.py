#!/usr/bin/env python3
"""Build the pinned official full-format 7-Zip library (including RAR decoding)."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
SOURCE = json.loads((ROOT / 'nix/sevenzip-source.json').read_text())


def build(output, source_archive=None, jobs=None):
    if sys.platform not in ('linux', 'darwin'):
        raise ValueError('Build 7z.dll on Windows with the official Format7zF MSVC makefile; see docs/native-sevenzip.md')
    output = Path(output).resolve()
    with tempfile.TemporaryDirectory(prefix='smartzip-sevenzip-') as temporary:
        work = Path(temporary)
        archive = Path(source_archive).resolve() if source_archive else work / 'source.tar.xz'
        if source_archive is None:
            with urllib.request.urlopen(SOURCE['url']) as response, archive.open('wb') as target:
                shutil.copyfileobj(response, target)
        with archive.open('rb') as stream:
            digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        if digest != SOURCE['sha256']:
            raise ValueError(f'7-Zip source SHA-256 mismatch: expected {SOURCE["sha256"]}, got {digest}')
        source = work / 'source'
        source.mkdir()
        with tarfile.open(archive, 'r:xz') as bundle:
            bundle.extractall(source, filter='data')
        directory = source / 'CPP/7zip/Bundles/Format7zF'
        command = ['make', '-f', '../../cmpl_gcc.mak', f'-j{jobs or os.cpu_count() or 1}',
                   'O=_o', 'USE_ASM=', 'CFLAGS_WARN_WALL=-Wall -Wextra', f'CC={os.environ.get("CC", "gcc")}',
                   f'CXX={os.environ.get("CXX", "g++")}']
        name = '7z.so'
        if sys.platform == 'darwin':
            arch = 'arm64' if platform.machine() == 'arm64' else 'x64'
            command[2] = f'../../cmpl_mac_{arch}.mak'
            command[-2:] = [f'CC={os.environ.get("CC", "clang")}',
                            f'CXX={os.environ.get("CXX", "clang++")}']
            command += ['SHARED_EXT=.dylib', 'LDFLAGS=-dynamiclib -fPIC',
                        'LIB2=-lpthread', 'LFLAGS_STRIP=']
            name = '7z.dylib'
        subprocess.run(command, cwd=directory, check=True)
        output.mkdir(parents=True, exist_ok=True)
        shutil.copy2(directory / '_o' / name, output / name)
        licenses = output / 'licenses/7zip'
        licenses.mkdir(parents=True, exist_ok=True)
        for filename in ('License.txt', 'copying.txt', 'unRarLicense.txt'):
            shutil.copy2(source / 'DOC' / filename, licenses / filename)
        (licenses / 'source.json').write_text(json.dumps(SOURCE, indent=2) + '\n')
        shutil.copy2(archive, output / '7zip-source.tar.xz')
    return output / name


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--source-archive', type=Path, help='Use the pinned source archive without downloading')
    parser.add_argument('--jobs', type=int)
    args = parser.parse_args()
    try:
        print(build(args.output, args.source_archive, args.jobs))
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f'7-Zip build failed: {error}\n')
