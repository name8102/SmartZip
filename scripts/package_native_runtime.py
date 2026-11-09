#!/usr/bin/env python3
"""Copy the native archive worker and redistributable 7-Zip materials; never download."""
import os
from pathlib import Path
import shutil
import sys

LIBRARY_NAMES = {'linux': '7z.so', 'darwin': '7z.dylib', 'win32': '7z.dll'}


def runtime_files(binary, worker=None, library=None, platform=None):
    platform = platform or sys.platform
    directory = Path(binary).resolve().parent
    worker = Path(worker or os.environ.get('SMARTZIP_ARCHIVE_WORKER')
                  or directory / ('smartzip-archive-worker.exe' if platform == 'win32'
                                  else 'smartzip-archive-worker'))
    library = Path(library or os.environ.get('SMARTZIP_7Z_LIBRARY')
                   or directory / 'lib' / LIBRARY_NAMES[platform])
    licenses = library.parent / 'licenses/7zip'
    source = library.parent / '7zip-source.tar.xz'
    if library.parent.parent.name == 'lib' and library.parent.name == 'smartzip':
        prefix = library.parents[2]
        licenses = prefix / 'share/licenses/7zip'
        source = prefix / 'share/smartzip/7zip-source.tar.xz'
    required = [worker, library, source] + [licenses / name for name in
                ('License.txt', 'copying.txt', 'unRarLicense.txt', 'source.json')]
    for path in required:
        if not path.is_file():
            raise ValueError(f'Missing native runtime material: {path}; build the archive worker '
                             'and run scripts/package_sevenzip.py --output <binary-directory>/lib')
    return worker, library, licenses, source


def copy_runtime(binary, destination, worker=None, library=None, platform=None):
    platform = platform or sys.platform
    worker, library, licenses, source = runtime_files(binary, worker, library, platform)
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    worker_name = 'smartzip-archive-worker.exe' if platform == 'win32' else 'smartzip-archive-worker'
    shutil.copy2(worker, destination / worker_name)
    (destination / worker_name).chmod(0o755)
    libraries = destination / 'lib'
    libraries.mkdir(exist_ok=True)
    shutil.copy2(library, libraries / LIBRARY_NAMES[platform])
    shutil.copy2(source, libraries / '7zip-source.tar.xz')
    shutil.copytree(licenses, libraries / 'licenses/7zip', dirs_exist_ok=True)
    return worker, library
