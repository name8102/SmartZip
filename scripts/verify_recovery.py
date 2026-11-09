#!/usr/bin/env python3
"""Kill and restart the native-library CLI at atomic commits in isolated directories.
Linux interrupts atomic commits using a test-only rename interposer.
No production failpoints, user state, or GUI windows are used.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    library_name = '7z.dll' if sys.platform == 'win32' else '7z.dylib' if sys.platform == 'darwin' else '7z.so'
    candidates = ([Path(os.environ['SMARTZIP_7Z_LIBRARY'])] if os.environ.get('SMARTZIP_7Z_LIBRARY') else
                  [binary.parent / library_name, binary.parent / 'lib' / library_name,
                   binary.parent.parent / 'lib/smartzip' / library_name])
    library = next((path.resolve() for path in candidates if path.is_file()), None)
    if library is None:
        raise SystemExit('REQUIRED: native 7-Zip library; set SMARTZIP_7Z_LIBRARY or use a packaged runtime')
    if sys.platform != 'linux':
        parser.error('commit fault injection requires Linux renameat2 interposition')
    checks = []
    with tempfile.TemporaryDirectory(prefix='smartzip-recovery-') as tmp:
        root = Path(tmp)
        interposer = root / 'commit_fault.so'
        subprocess.run(['cc', '-shared', '-fPIC', str(ROOT/'scripts/fixtures/commit_fault.c'), '-ldl', '-o', str(interposer)], check=True)
        phases = ['after-backup', 'before-publish', 'after-publish']
        for phase in phases:
            work = root / phase
            work.mkdir()
            marker = work/'ready'
            config = work/'config.toml'
            config.write_text('schema_version = 1\ndefaults_version = 1\n[backends]\nauto_discover = false\n[[backends.installations]]\nid = "real-seven"\nfamily = "seven-zip-library"\npath = ' + json.dumps(str(library)) + '\n')
            db = work/'state.db'
            archive = work/'source.zip'
            with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as z:
                z.writestr('中文.txt', b'new contents\n' * 8192)
            source_hash = hashlib.sha256(archive.read_bytes()).hexdigest()
            output = work/'output'
            target = output/'source'
            target.mkdir(parents=True)
            (target/'old.txt').write_text('preserve old output until commit')
            env = dict(os.environ, XDG_CONFIG_HOME=str(work/'config'), XDG_DATA_HOME=str(work/'data'))
            base = [str(binary), '--config', str(config), '--db', str(db)]
            command = base + ['extract', str(archive), '--output', str(output), '--layout', 'raw', '--on-conflict', 'overwrite', '--json']
            crash_env = dict(env)
            crash_env.update(LD_PRELOAD=str(interposer), SZ_FAULT_TARGET=str(target), SZ_FAULT_PHASE=phase, SZ_FAULT_MARKER=str(marker))
            with (work/'first.stdout').open('wb') as stdout, (work/'first.stderr').open('wb') as stderr:
                process = subprocess.Popen(command, stdout=stdout, stderr=stderr, stdin=subprocess.DEVNULL, env=crash_env)
                try:
                    deadline = time.monotonic() + 30
                    while not marker.exists() and process.poll() is None and time.monotonic() < deadline:
                        time.sleep(0.01)
                    assert marker.exists(), (phase, process.poll(), (work/'first.stderr').read_text(), (work/'first.stdout').read_text()[-2000:])
                    with sqlite3.connect(db) as connection:
                        task_id = connection.execute("SELECT id FROM tasks WHERE kind='extract'").fetchone()[0]
                        assert connection.execute('SELECT finished_at FROM tasks WHERE id=?', (task_id,)).fetchone()[0] is None
                    process.kill()
                    process.wait(timeout=10)
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait(timeout=10)
            if phase in ['after-backup', 'before-publish']:
                assert not target.exists()
                assert len(list(output.glob('.smartzip-backup-*/original/old.txt'))) == 1
            else:
                assert (target/'中文.txt').read_bytes() == b'new contents\n' * 8192
            # A different input triggers recovery; it must not stand in for the
            # interrupted archive or hide duplicate execution via a new request.
            trigger = work/'trigger.zip'
            with zipfile.ZipFile(trigger, 'w') as z:
                z.writestr('trigger.txt', 'separate request')
            result = subprocess.run(base + ['extract', str(trigger), '--output', str(work/'trigger-out'), '--json'], env=env, capture_output=True, text=True, timeout=40)
            assert result.returncode == 0, (phase, result.stdout[-2000:], result.stderr)
            assert (target/'中文.txt').read_bytes() == b'new contents\n' * 8192, phase
            assert not (target/'old.txt').exists()
            assert not list(output.glob('.smartzip-*')), (phase, list(output.iterdir()))
            with sqlite3.connect(db) as connection:
                task = connection.execute('SELECT status, committed_output_bytes, finished_at FROM tasks WHERE id=?', (task_id,)).fetchone()
                assert task[0] == 'completed' and task[1] == len(b'new contents\n' * 8192) and task[2], (phase, task)
                rows = connection.execute('SELECT status FROM file_extractions WHERE task_id=?', (task_id,)).fetchall()
                assert rows == [('extracted',)], (phase, rows)
            assert hashlib.sha256(archive.read_bytes()).hexdigest() == source_hash
            checks.append(f'{phase}: SIGKILL, restart, exact output/history/budget, no staging or backup leak')
    print(json.dumps({'passed': len(checks), 'checks': checks, 'commit_fault_platform': 'Linux renameat2 interposition; other platforms require native commit acceptance'}, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
