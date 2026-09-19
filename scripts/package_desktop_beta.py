#!/usr/bin/env python3
"""Package GUI + CLI for the host platform without installation or GUI startup."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {'linux': 'x86_64-unknown-linux-gnu', 'darwin': 'aarch64-apple-darwin'}


def package(cli, gui, target, destination):
    if TARGETS.get(sys.platform) != target:
        raise ValueError('Package on the matching native platform; cross-host signing is unsupported')
    cli, gui, destination = Path(cli).resolve(), Path(gui).resolve(), Path(destination)
    for binary in (cli, gui):
        if not binary.is_file():
            raise ValueError(f'Missing executable: {binary}')
    version = tomllib.loads((ROOT/'crates/smartzip-cli/Cargo.toml').read_text())['package']['version']
    gui_version = tomllib.loads((ROOT/'crates/smartzip-gui/Cargo.toml').read_text())['package']['version']
    actual = subprocess.check_output([str(cli), '--version'], text=True).strip().split()[-1]
    if actual != version or gui_version != version:
        raise ValueError('CLI binary, CLI manifest and GUI manifest versions must agree')
    name = f'smartzip-desktop-{version}-{target}'
    destination.mkdir(parents=True, exist_ok=True)
    archive = destination/f'{name}.tar.gz'
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)/name
        (root/'bin').mkdir(parents=True)
        shutil.copy2(cli, root/'bin/smartzip')
        if sys.platform == 'darwin':
            spec = importlib.util.spec_from_file_location('package_macos', ROOT/'scripts/package-macos.py')
            packager = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(packager)
            packager.package(gui, root/'SmartZip.app')
            shutil.copy2(ROOT/'scripts/install-macos.py', root/'install-macos.py')
            shutil.copy2(ROOT/'scripts/package-macos.py', root/'package-macos.py')
        else:
            shutil.copy2(gui, root/'bin/smartzip-gui')
            shutil.copy2(ROOT/'scripts/install_linux.py', root/'install-linux.py')
        for source in ['README.md', 'LICENSE', 'CHANGELOG.md', 'docs/cli-beta.md', 'docs/desktop-beta.md']:
            shutil.copy2(ROOT/source, root/Path(source).name)
        dependency_command = ['otool', '-L'] if sys.platform == 'darwin' else ['ldd']
        dependencies = []
        for binary in (cli, gui):
            output = subprocess.check_output(dependency_command + [str(binary)], text=True)
            if 'not found' in output:
                raise ValueError(f'Unresolved runtime dependencies: {binary}')
            dependencies.append(output)
        (root/'dynamic-dependencies.txt').write_text('\n'.join(dependencies))
        build_system = platform.freedesktop_os_release().get('PRETTY_NAME') if sys.platform == 'linux' else platform.mac_ver()[0]
        (root/'release.json').write_text(json.dumps({'version': version, 'target': target, 'bundled_backend': False, 'build_system': build_system, 'build_libc': platform.libc_ver()}, indent=2)+'\n')
        with tarfile.open(archive, 'w:gz') as output:
            output.add(root, arcname=name)
    with archive.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    archive.with_suffix('.gz.sha256').write_text(f'{digest}  {archive.name}\n')
    return archive


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('cli', type=Path)
    parser.add_argument('gui', type=Path)
    parser.add_argument('target', choices=TARGETS.values())
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    print(package(args.cli, args.gui, args.target, args.destination))
