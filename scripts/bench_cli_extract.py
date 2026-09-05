#!/usr/bin/env python3
"""Paired warm-file-system extraction samples with fresh destinations and DBs."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import tempfile
import time
import zipfile

parser = argparse.ArgumentParser()
parser.add_argument('baseline', type=Path)
parser.add_argument('optimized', type=Path)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--samples', type=int, default=7)
args = parser.parse_args()
binaries = {'baseline': args.baseline.resolve(), 'optimized': args.optimized.resolve()}
seven = shutil.which('7z') or shutil.which('7zz')
assert seven, 'real 7-Zip is required'
results = {'platform': platform.platform(), 'files': 3000, 'samples': args.samples,
           'scope': 'synthetic ZIP with 3000 128-byte files; warm filesystem; CLI startup, test-before-extract, layout and nested scan included; fixture and DB initialization excluded',
           'binaries': {k: {'path': str(v), 'sha256': hashlib.sha256(v.read_bytes()).hexdigest()} for k, v in binaries.items()},
           'timings_seconds': {k: {'raw': []} for k in binaries}}
with tempfile.TemporaryDirectory(prefix='smartzip-extract-bench-') as workspace:
    root = Path(workspace)
    config = root / 'config.toml'
    config.write_text('[backends]\nauto_discover = false\n[[backends.installations]]\nid = "benchmark-7z"\nfamily = "seven-zip-cli"\nexecutable = ' + json.dumps(seven) + '\n')
    archive = root / 'small-files.zip'
    payload = b'x' * 128
    with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as z:
        for i in range(results['files']):
            z.writestr(f'file-{i:05d}.txt', payload)
    for sample in range(args.samples):
        # Alternate order to reduce drift correlated with the executable.
        for variant in (list(binaries) if sample % 2 == 0 else list(reversed(binaries))):
            db = root / f'{variant}-{sample}.db'
            base = [str(binaries[variant]), '--config', str(config), '--db', str(db)]
            subprocess.run(base + ['password', 'list', '--json'], capture_output=True, check=True)
            dest = root / f'{variant}-{sample}'
            started = time.perf_counter()
            run = subprocess.run(base + ['extract', str(archive), '--output', str(dest), '--layout', 'raw', '--json'], capture_output=True, check=True, timeout=60)
            elapsed = time.perf_counter() - started
            assert json.loads(run.stdout)['processed_count'] == 1
            outputs = list((dest / 'small-files').iterdir())
            assert len(outputs) == results['files']
            assert all(p.read_bytes() == payload for p in outputs)
            results['timings_seconds'][variant]['raw'].append(elapsed)
            shutil.rmtree(dest)
for times in results['timings_seconds'].values():
    times['median'] = statistics.median(times['raw'])
args.output.parent.mkdir(parents=True, exist_ok=True)
args.output.write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps(results, indent=2))
