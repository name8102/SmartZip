#!/usr/bin/env python3
"""Build, package and install SmartZip for the current macOS user."""
import argparse
import importlib.util
import json
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile

SPEC = importlib.util.spec_from_file_location("package_macos", Path(__file__).with_name("package-macos.py"))
PACKAGER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGER)


def validate_destination(destination):
    if destination.suffix != ".app" or destination.is_symlink():
        raise ValueError("安装目标必须是非符号链接的 .app 路径")
    if destination.exists():
        try:
            with (destination / "Contents/Info.plist").open("rb") as file:
                info = plistlib.load(file)
        except (OSError, plistlib.InvalidFileException) as error:
            raise ValueError(f"拒绝覆盖未知目录：{destination}") from error
        if not isinstance(info, dict) or info.get("CFBundleIdentifier") != PACKAGER.BUNDLE_ID:
            raise ValueError(f"拒绝覆盖其他应用：{destination}")


def install_bundle(source, destination, verify=True):
    source, destination = Path(source), Path(destination)
    validate_destination(destination)
    if source.resolve() == destination.resolve():
        return destination
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".smartzip-install-", dir=destination.parent) as temporary:
        staging = Path(temporary) / "SmartZip.app"
        shutil.copytree(source, staging, symlinks=True)
        if verify:
            subprocess.run(["/usr/bin/codesign", "--verify", "--strict", str(staging)], check=True)
        # Keep the old application until the staged replacement is ready.
        backup = Path(temporary) / "previous.app"
        if destination.exists():
            destination.rename(backup)
        try:
            staging.rename(destination)
        except BaseException:
            if backup.exists():
                backup.rename(destination)
            raise
    return destination


def build(profile):
    command = ["cargo", "build", "--locked", "-p", "smartzip-gui", "--message-format=json-render-diagnostics"]
    if profile == "release":
        command.append("--release")
    result = subprocess.run(command, cwd=PACKAGER.ROOT, stdout=subprocess.PIPE, text=True, check=True)
    # Cargo reports the actual executable, including CARGO_TARGET_DIR and target triples.
    executable = None
    for line in result.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (message.get("reason") == "compiler-artifact"
                and message.get("target", {}).get("name") == "smartzip-gui"
                and message.get("executable")):
            executable = Path(message["executable"])
    if executable is None:
        raise RuntimeError("Cargo 未返回 smartzip-gui 可执行文件")
    return executable


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=["debug", "release"], default="release")
    parser.add_argument("--destination", type=Path, default=Path.home() / "Applications/SmartZip.app")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("此安装脚本仅适用于 macOS")
    destination = args.destination.expanduser().absolute()
    validate_destination(destination)
    executable = build(args.profile)
    bundle = PACKAGER.package(executable, executable.parent / "bundle/SmartZip.app")
    install_bundle(bundle, destination)
    print(f"已安装：{destination}")
    print("启动该应用，在「系统集成」中配置默认打开方式和 Finder 右键菜单。")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"安装失败：{error}", file=sys.stderr)
        sys.exit(1)
