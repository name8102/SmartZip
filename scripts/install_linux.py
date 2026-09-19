#!/usr/bin/env python3
"""Install a portable Linux beta in a dedicated directory; preserve user state.
No root privileges, file associations, PATH or desktop preferences are changed.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tempfile

MARKER = '.smartzip-install.json'


def manifest(root):
    files = {}
    for path in sorted(root.rglob('*')):
        if path.is_symlink():
            raise ValueError(f'Refusing symbolic link: {path}')
        if path.is_dir():
            files[str(path.relative_to(root)) + "/"] = None
        elif path.is_file() and path != root/MARKER:
            with path.open('rb') as stream:
                files[str(path.relative_to(root))] = hashlib.file_digest(stream, 'sha256').hexdigest()
    return files


def validate(destination):
    if destination.is_symlink():
        raise ValueError('Refusing a symbolic-link destination')
    if destination.exists():
        try:
            saved = json.loads((destination/MARKER).read_text())
        except (OSError, ValueError) as error:
            raise ValueError('Refusing an unknown installation directory') from error
        if saved.get('application') != 'org.smartzip.SmartZip' or saved.get('files') != manifest(destination):
            raise ValueError('Installed files were modified; preserve them before updating or uninstalling')


def install(source, destination):
    source, destination = Path(source).resolve(), Path(destination).absolute()
    validate(destination)
    if source == destination or source.is_relative_to(destination) or destination.is_relative_to(source):
        raise ValueError('Source and destination must be separate directories')
    for name in ['bin/smartzip', 'bin/smartzip-gui', 'release.json']:
        if not (source/name).is_file():
            raise ValueError(f'Incomplete package: {name}')
    manifest(source)  # refuse links before copying
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.smartzip-install-', dir=destination.parent) as tmp:
        staging, backup = Path(tmp)/'new', Path(tmp)/'old'
        shutil.copytree(source, staging)
        (staging/MARKER).write_text(json.dumps({'application': 'org.smartzip.SmartZip', 'files': manifest(staging)}, indent=2)+'\n')
        if destination.exists():
            destination.rename(backup)
        try:
            staging.rename(destination)
        except BaseException:
            if backup.exists():
                backup.rename(destination)
            raise
    return destination


def uninstall(destination):
    destination = Path(destination)
    validate(destination)
    if destination.exists():
        shutil.rmtree(destination)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=Path(__file__).resolve().parent)
    parser.add_argument('--destination', type=Path, default=Path.home()/'.local/opt/SmartZip')
    parser.add_argument('--uninstall', action='store_true')
    args = parser.parse_args()
    destination = args.destination.expanduser().absolute()
    if args.uninstall:
        uninstall(destination)
    else:
        print(install(args.source, destination))
