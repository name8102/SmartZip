#!/usr/bin/env python3
"""Build a local application bundle; never install or change file associations."""
import argparse
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
BUNDLE_ID = "org.smartzip.SmartZip"


def info_plist():
    formats = json.loads((ROOT / "resources/file-types.json").read_text())
    return {
        "CFBundleIdentifier": BUNDLE_ID,
        "CFBundleExecutable": "smartzip-gui",
        "CFBundleName": "SmartZip",
        "CFBundleDisplayName": "SmartZip",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": "0.1.0",
        "CFBundleVersion": "1",
        "LSMinimumSystemVersion": "12.0",
        "NSHighResolutionCapable": True,
        "CFBundleURLTypes": [{"CFBundleURLName": BUNDLE_ID, "CFBundleURLSchemes": ["smartzip"], "CFBundleTypeRole": "Viewer"}],
        "CFBundleDocumentTypes": [{
            "CFBundleTypeName": item["label"] + " archive",
            "CFBundleTypeRole": "Viewer", "LSHandlerRank": "Alternate",
            "CFBundleTypeExtensions": [item["extension"]],
            "LSItemContentTypes": [item["uti"]],
        } for item in formats],
        "UTImportedTypeDeclarations": [{
            "UTTypeIdentifier": item["uti"],
            "UTTypeDescription": item["label"] + " archive",
            "UTTypeConformsTo": ["public.archive", "public.data"],
            "UTTypeTagSpecification": {"public.filename-extension": [item["extension"]], "public.mime-type": item["mime"]},
        } for item in formats],
    }


def package(binary, output, sign=True):
    binary, output = Path(binary), Path(output)
    if not binary.is_file():
        raise ValueError(f"Build the GUI first: missing {binary}")
    if output.is_symlink():
        raise ValueError("Refusing to replace a symbolic-link bundle")
    if output.exists():
        try:
            with (output / "Contents/Info.plist").open("rb") as source:
                old = plistlib.load(source)
        except (OSError, plistlib.InvalidFileException) as error:
            raise ValueError("Refusing to replace an unknown bundle") from error
        if old.get("CFBundleIdentifier") != BUNDLE_ID:
            raise ValueError("Refusing to replace a different application's bundle")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".smartzip-package-", dir=output.parent) as temporary:
        staging = Path(temporary) / "SmartZip.app"
        executable = staging / "Contents/MacOS/smartzip-gui"
        executable.parent.mkdir(parents=True)
        shutil.copy2(binary, executable)
        executable.chmod(0o755)
        with (staging / "Contents/Info.plist").open("wb") as target:
            plistlib.dump(info_plist(), target)
        if sign and sys.platform == "darwin":
            subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", str(staging)], check=True)
            subprocess.run(["/usr/bin/codesign", "--verify", "--strict", str(staging)], check=True)
        backup = Path(temporary) / "previous.app"
        if output.exists():
            output.rename(backup)
        try:
            staging.rename(output)
        except BaseException:
            if backup.exists():
                backup.rename(output)
            raise
    return output


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=["debug", "release"], default="release")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    binary = args.binary or ROOT / "target" / args.profile / "smartzip-gui"
    output = args.output or ROOT / "target" / args.profile / "bundle" / "SmartZip.app"
    print(package(binary, output))
