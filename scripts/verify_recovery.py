#!/usr/bin/env python3
"""Kill and restart the actual CLI against real 7-Zip in isolated directories.
Linux also interrupts atomic commits using a test-only rename interposer.
No production failpoints, user state, or GUI windows are used.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
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
    seven = shutil.which('7z') or shutil.which('7zz')
    assert seven, 'real 7-Zip required'
    checks = []
    with tempfile.TemporaryDirectory(prefix='smartzip-recovery-') as tmp:
        root = Path(tmp)
        interposer = root / 'commit_fault.so'
        phases = ['extract']
        if sys.platform == 'linux':
            subprocess.run(['cc', '-shared', '-fPIC', str(ROOT/'scripts/fixtures/commit_fault.c'), '-ldl', '-o', str(interposer)], check=True)
            phases += ['after-backup', 'before-publish', 'after-publish']
        for phase in phases:
            work = root / phase
            work.mkdir()
            marker, fault = work/'ready', work/'enabled'
            fault.touch()
            wrapper = work/'7z-wrapper'
            # Let the real backend finish writing staging, but not return success
            # to SmartZip. Killing here exercises abandoned extraction ownership.
            wrapper.write_text(f'''#!{sys.executable}
import os, pathlib, signal, subprocess, sys
result = subprocess.run([{seven!r}] + sys.argv[1:])
if sys.argv[1:2] == ['x'] and pathlib.Path({str(fault)!r}).exists():
    pathlib.Path({str(marker)!r}).write_text(str(os.getpid()))
    os.kill(os.getpid(), signal.SIGSTOP)
sys.exit(result.returncode)
''')
            wrapper.chmod(0o755)
            config = work/'config.toml'
            backend = str(wrapper) if phase == 'extract' else seven
            config.write_text('schema_version = 1\ndefaults_version = 1\n[backends]\nauto_discover = false\n[[backends.installations]]\nid = "real-seven"\nfamily = "seven-zip-cli"\nexecutable = ' + json.dumps(backend) + '\n')
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
            if phase != 'extract':
                crash_env.update(LD_PRELOAD=str(interposer), SZ_FAULT_TARGET=str(target), SZ_FAULT_PHASE=phase, SZ_FAULT_MARKER=str(marker))
            with (work/'first.stdout').open('wb') as stdout, (work/'first.stderr').open('wb') as stderr:
                process = subprocess.Popen(command, stdout=stdout, stderr=stderr, stdin=subprocess.DEVNULL, env=crash_env)
                try:
                    deadline = time.monotonic() + 30
                    while not marker.exists() and process.poll() is None and time.monotonic() < deadline:
                        time.sleep(0.01)
                    assert marker.exists(), (phase, process.poll(), (work/'first.stderr').read_text(), (work/'first.stdout').read_text()[-2000:])
                    if phase == 'extract':
                        preview = subprocess.run(base + ['list', str(archive), '--json'], env=env, capture_output=True, text=True, timeout=15)
                        assert preview.returncode == 0 and '中文.txt' in preview.stdout, preview.stderr
                        contender = subprocess.run(command, env=env, capture_output=True, text=True, timeout=15)
                        assert contender.returncode == 1, contender.stdout
                        assert process.poll() is None
                        checks.append('during held real extraction: listing remains usable; second execution owner is rejected')
                    with sqlite3.connect(db) as connection:
                        task_id = connection.execute("SELECT id FROM tasks WHERE kind='extract'").fetchone()[0]
                        assert connection.execute('SELECT finished_at FROM tasks WHERE id=?', (task_id,)).fetchone()[0] is None
                    process.kill()
                    process.wait(timeout=10)
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait(timeout=10)
                    if phase == 'extract' and marker.exists():
                        try:
                            os.kill(int(marker.read_text()), signal.SIGKILL)
                        except ProcessLookupError:
                            pass
            fault.unlink()
            if phase in ['after-backup', 'before-publish']:
                assert not target.exists()
                assert len(list(output.glob('.smartzip-backup-*/original/old.txt'))) == 1
            elif phase == 'extract':
                assert (target/'old.txt').read_text() == 'preserve old output until commit'
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
