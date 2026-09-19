#!/usr/bin/env python3
"""Bounded release stress checks; not a throughput benchmark or an OS quota test."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import zipfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    seven = shutil.which('7z') or shutil.which('7zz')
    assert seven, 'real 7-Zip required'
    checks, measurements = [], []
    with tempfile.TemporaryDirectory(prefix='smartzip-resource-') as tmp:
        root = Path(tmp)
        config = root/'config.toml'
        config.write_text('schema_version = 1\ndefaults_version = 1\n[backends]\nauto_discover = false\n[[backends.installations]]\nid = "seven"\nfamily = "seven-zip-cli"\nexecutable = ' + json.dumps(seven) + '\n')
        base = [str(binary), '--config', str(config), '--db', str(root/'state.db')]
        env = dict(os.environ, XDG_CONFIG_HOME=str(root/'config'), XDG_DATA_HOME=str(root/'data'))

        def run(arguments, expected=0):
            with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
                start = time.monotonic()
                process = subprocess.Popen(base + list(map(str, arguments)) + ['--json'], stdout=stdout, stderr=stderr, stdin=subprocess.DEVNULL, env=env)
                peak = 0
                try:
                    while process.poll() is None:
                        if sys.platform == 'linux':
                            try:
                                for line in Path(f'/proc/{process.pid}/status').read_text().splitlines():
                                    if line.startswith('VmRSS:'):
                                        peak = max(peak, int(line.split()[1]))
                            except FileNotFoundError:
                                pass
                        assert time.monotonic()-start < 90, 'bounded fixture exceeded 90 seconds'
                        time.sleep(0.01)
                    stdout.seek(0)
                    stderr.seek(0)
                    out, err = stdout.read().decode(), stderr.read().decode()
                    assert process.returncode == expected, (arguments, process.returncode, out[-3000:], err)
                    measurements.append({'operation': arguments[0], 'seconds': round(time.monotonic()-start, 3), 'sampled_cli_peak_rss_kib': peak or None})
                    return json.loads(out)
                finally:
                    if process.poll() is None:
                        process.kill()
                    process.wait()

        small = root/'small.zip'
        with zipfile.ZipFile(small, 'w') as z:
            z.writestr('payload.txt', 'after empty scan windows')
        carrier = root/'long-prefix.bin'
        with carrier.open('wb') as stream:
            stream.seek(192 * 1024 * 1024)
            stream.write(small.read_bytes())
        report = run(['detect', carrier])
        assert 'Zip' in json.dumps(report) or 'zip' in json.dumps(report), report
        if measurements[-1]['sampled_cli_peak_rss_kib']:
            assert measurements[-1]['sampled_cli_peak_rss_kib'] < 128 * 1024, measurements[-1]
        checks.append('192 MiB empty prefix: finds trailing ZIP; Linux sampled CLI RSS below 128 MiB')

        many = root/'many.zip'
        with zipfile.ZipFile(many, 'w', zipfile.ZIP_DEFLATED) as z:
            for i in range(4000):
                z.writestr(f'{i:04}.txt', f'file {i}')
        output = root/'many-output'
        target = output/'many'
        target.mkdir(parents=True)
        (target/'old.txt').write_text('old')
        run(['extract', many, '--output', output, '--layout', 'raw', '--on-conflict', 'overwrite', '--max-files', '2000'], expected=1)
        assert list(target.iterdir()) == [target/'old.txt'] and (target/'old.txt').read_text() == 'old'
        assert not list(output.glob('.smartzip-*'))
        checks.append('4000 small files with 2000-entry cap: fails safely, preserves previous output, cleans staging')

        archives = []
        for i in range(3):
            archive = root/f'root-{i}.zip'
            with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as z:
                z.writestr('data.bin', bytes([65+i]) * (2*1024*1024))
            archives.append(archive)
        output = root/'batch'
        report = run(['extract', *archives, '--output', output, '--layout', 'raw', '--max-output-bytes', str(3*1024*1024)], expected=2)
        assert report['processed_count'] == 1 and report['failed_count'] == 2, report
        assert sum(p.stat().st_size for p in output.rglob('*') if p.is_file()) == 2*1024*1024
        assert not list(output.glob('.smartzip-*'))
        checks.append('three roots share a 3 MiB task budget: only one 2 MiB output commits')
    print(json.dumps({'passed': len(checks), 'checks': checks, 'measurements': measurements, 'scope': 'bounded synthetic cases; RSS samples cover CLI only, not child backends; no throughput claim'}, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
