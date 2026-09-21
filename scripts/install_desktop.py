#!/usr/bin/env python3
"""Build and install the native CLI and desktop application for the current user."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts' / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def build(profile):
    command = ['cargo', 'build', '--locked', '-p', 'smartzip-cli', '-p', 'smartzip-gui',
               '--message-format=json-render-diagnostics']
    if profile == 'release':
        command.append('--release')
    result = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE, text=True, check=True)
    binaries = {}
    for line in result.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        name = message.get('target', {}).get('name')
        if (message.get('reason') == 'compiler-artifact'
                and name in ('smartzip', 'smartzip-gui') and message.get('executable')):
            binaries[name] = Path(message['executable'])
    if len(binaries) != 2:
        raise RuntimeError('Cargo 未返回 CLI 和 GUI 可执行文件')
    return binaries


def desktop_exec(path):
    # Desktop Entry Exec uses its own quoting rules, not shell quoting.
    value = str(path).replace('%', '%%')
    for character in ('\\', '"', '`', '$'):
        value = value.replace(character, '\\' + character)
    return '"' + value.replace('\\', '\\\\') + '" %F'


def install_linux(binaries, destination):
    installer = load('install_linux', 'install_linux.py')
    bundle = binaries['smartzip-gui'].parent / 'bundle/SmartZip'
    # Build a fresh package, then use the existing installer for staged updates.
    with tempfile.TemporaryDirectory(prefix='smartzip-package-') as temporary:
        source = Path(temporary)
        (source / 'bin').mkdir()
        for name, binary in binaries.items():
            shutil.copy2(binary, source / 'bin' / name)
        version = subprocess.check_output([str(binaries['smartzip']), '--version'], text=True).strip()
        (source / 'release.json').write_text(json.dumps({
            'version': version.removeprefix('smartzip '), 'platform': sys.platform,
            'bundled_backend': False,
        }, indent=2) + '\n')
        installer.install(source, bundle)
    installer.install(bundle, destination)
    data_home = Path(os.environ.get('XDG_DATA_HOME') or Path.home() / '.local/share')
    launcher = data_home / 'applications/org.smartzip.SmartZip.desktop'
    launcher.parent.mkdir(parents=True, exist_ok=True)
    launcher.write_text('[Desktop Entry]\nType=Application\nName=SmartZip\n'
                        'Comment=Extract and browse archives\n'
                        f'Exec={desktop_exec(destination / "bin/smartzip-gui")}\n'
                        'Terminal=false\nCategories=Utility;Archiving;\n')
    return bundle


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', choices=['debug', 'release'], default='release')
    parser.add_argument('--destination', default='', help='GUI installation directory')
    parser.add_argument('--gui-only', action='store_true')
    args = parser.parse_args(argv)
    if sys.platform not in ('linux', 'darwin'):
        parser.error('当前支持 Linux 和 macOS 原生安装')
    default = '~/.local/opt/SmartZip' if sys.platform == 'linux' else '~/Applications/SmartZip.app'
    destination = Path(args.destination or default).expanduser().absolute()
    binaries = build(args.profile)
    if sys.platform == 'darwin':
        installer = load('install_macos', 'install-macos.py')
        bundle = installer.PACKAGER.package(binaries['smartzip-gui'],
                                           binaries['smartzip-gui'].parent / 'bundle/SmartZip.app')
        installer.install_bundle(bundle, destination)
    else:
        bundle = install_linux(binaries, destination)
    if not args.gui_only:
        command = ['cargo', 'install', '--path', 'crates/smartzip-cli', '--bin', 'smartzip',
                   '--locked', '--force']
        if args.profile == 'debug':
            command.append('--debug')
        subprocess.run(command, cwd=ROOT, check=True)
    print(f'GUI 安装位置：{destination}\nGUI 打包产物：{bundle}')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f'安装失败：{error}', file=sys.stderr)
        sys.exit(1)
