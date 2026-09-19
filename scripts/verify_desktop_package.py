#!/usr/bin/env python3
"""Verify unpacking and isolated install/update/remove; never launch GUI windows."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile


def load(path):
    spec = importlib.util.spec_from_file_location('installer', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path)
    args = parser.parse_args()
    archive = args.archive.resolve()
    with archive.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    assert archive.with_suffix('.gz.sha256').read_text().split()[0] == digest
    with tempfile.TemporaryDirectory(prefix='smartzip-install-check-') as tmp:
        root = Path(tmp)
        with tarfile.open(archive) as source:
            source.extractall(root/'unpacked', filter='data')
        packages = list((root/'unpacked').iterdir())
        assert len(packages) == 1
        package = packages[0]
        info = json.loads((package/'release.json').read_text())
        config = root/'config.toml'
        config.write_text('schema_version = 1\ndefaults_version = 1\n')
        env = dict(os.environ, XDG_CONFIG_HOME=str(root/'config'), XDG_DATA_HOME=str(root/'data'))
        def doctor(binary):
            version = subprocess.check_output([str(binary), '--version'], text=True).strip().split()[-1]
            assert version == info['version']
            subprocess.run([str(binary), '--config', str(config), '--db', str(root/'private.db'), 'doctor', '--json'], env=env, check=True, capture_output=True)
            subprocess.run([str(binary), '--config', str(config), '--db', str(root/'private.db'), 'history', 'tasks', '--json'], env=env, check=True, capture_output=True)
            assert (root/'private.db').is_file()
        doctor(package/'bin/smartzip')
        if sys.platform == 'linux':
            installer = load(package/'install-linux.py')
            destination = root/'installed with spaces/SmartZip'
            installer.install(package, destination)
            doctor(destination/'bin/smartzip')
            installer.install(package, destination)
            doctor(destination/'bin/smartzip')
            # Removing binaries must not remove the separate state database.
            installer.uninstall(destination)
            assert not destination.exists() and (root/'private.db').exists()
        elif sys.platform == 'darwin':
            installer = load(package/'install-macos.py')
            destination = root/'installed with spaces/SmartZip.app'
            installer.install_bundle(package/'SmartZip.app', destination)
            installer.install_bundle(package/'SmartZip.app', destination)
            subprocess.run(['codesign', '--verify', '--strict', str(destination)], check=True)
            # Deleting only the application is the documented macOS uninstall.
            import shutil
            shutil.rmtree(destination)
            assert (root/'private.db').exists()
        else:
            raise AssertionError('unsupported release host')
    print(json.dumps({'verified': ['checksum', 'unpack', 'CLI version and doctor', 'isolated install and update', 'uninstall preserves state'], 'native_gui': 'user acceptance required'}, indent=2))


if __name__ == '__main__':
    main()
