#!/usr/bin/env python3
"""Package only the verified CLI and its runtime/install documentation."""
import hashlib
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import shutil

binary, target, destination = Path(sys.argv[1]).resolve(), sys.argv[2], Path(sys.argv[3])
version = subprocess.check_output([str(binary), '--version'], text=True).strip().split()[-1]
name = f'smartzip-{version}-{target}'
destination.mkdir(parents=True, exist_ok=True)
archive = destination / f'{name}.tar.gz'
with tempfile.TemporaryDirectory() as work:
    root = Path(work) / name
    root.mkdir()
    shutil.copy2(binary, root / 'smartzip')
    for source in ['README.md', 'LICENSE', 'CHANGELOG.md', 'docs/cli-beta.md']:
        shutil.copy2(source, root / Path(source).name)
    with tarfile.open(archive, 'w:gz') as output:
        output.add(root, arcname=name)
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
archive.with_suffix(archive.suffix + '.sha256').write_text(f'{digest}  {archive.name}\n')
print(archive)
