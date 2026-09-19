#!/usr/bin/env python3
"""Real backend regressions for history reuse, credential evidence and CLI prompts."""
import argparse
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import time
import zipfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary', type=Path)
    args = parser.parse_args()
    binary = str(args.binary.resolve())
    seven = shutil.which('7z') or shutil.which('7zz')
    assert seven, 'real 7-Zip required'
    checks = []
    with tempfile.TemporaryDirectory(prefix='smartzip-review-fixes-') as tmp:
        root = Path(tmp)
        config = root / 'config.toml'
        config.write_text('schema_version = 1\ndefaults_version = 1\n[backends]\nauto_discover = false\n[[backends.installations]]\nid = "seven"\nfamily = "seven-zip-cli"\nexecutable = ' + json.dumps(seven) + '\n')
        db = root / 'state.db'
        env = dict(os.environ, XDG_DATA_HOME=str(root/'data'), XDG_CONFIG_HOME=str(root/'config'))
        base = [binary, '--config', str(config), '--db', str(db)]

        def run(arguments, settings=(), expected=0):
            command = base + [x for setting in settings for x in ['--set', setting]] + list(map(str, arguments))
            result = subprocess.run(command, input='', capture_output=True, text=True, env=env, timeout=30)
            assert result.returncode == expected, (command, result.returncode, result.stderr, result.stdout[:1000])
            return json.loads(result.stdout) if '--json' in arguments else result

        archive = root/'sample.zip'
        with zipfile.ZipFile(archive, 'w') as z:
            z.writestr('file.txt', 'history regression')
        on = ['extraction.reuse.skip_completed=true']
        run(['list', archive, '--password', 'not-a-password', '--json'])
        with sqlite3.connect(db) as connection:
            assert connection.execute('SELECT COUNT(*) FROM known_files WHERE password_id IS NOT NULL OR last_extract_at IS NOT NULL').fetchone()[0] == 0
        first = root/'first'
        report = run(['extract', archive, '--output', first, '--json'], on)
        assert report['processed_count'] == 1, report
        shutil.rmtree(first)
        # Hints are disabled to prove canonical history alone supplies completion.
        report = run(['extract', archive, '--output', root/'second', '--json'], on + ['state.known_files="off"'])
        assert report['processed_count'] == 0 and report['skipped_count'] == 1, report
        assert any(e['kind'].get('Decision', {}).get('reason') == 'already_extracted' for e in report['events'] if isinstance(e['kind'], dict))
        assert not (root/'second'/'sample').exists()
        for settings, extra, output in [(on, ['--force'], 'forced'), (['extraction.reuse.skip_completed=false'], [], 'disabled')]:
            report = run(['extract', archive, '--output', root/output, '--json'] + extra, settings)
            assert report['processed_count'] == 1, report
        with sqlite3.connect(db) as connection:
            count = connection.execute('SELECT COUNT(*) FROM tasks').fetchone()[0]
        report = run(['extract', archive, '--output', root/'no-history', '--json'], on + ['state.history=false'])
        assert report['skipped_count'] == 1, report
        with sqlite3.connect(db) as connection:
            assert connection.execute('SELECT COUNT(*) FROM tasks').fetchone()[0] == count
            assert connection.execute('SELECT COUNT(*) FROM known_files WHERE last_extract_at IS NOT NULL').fetchone()[0] == 0
            connection.execute("DELETE FROM tasks WHERE kind = 'extract'")
        report = run(['extract', archive, '--output', root/'after-clear', '--json'], on)
        assert report['processed_count'] == 1, report
        checks.append('one history: list is not completion; missing output, hints off, force, config off, history read-only and clear')

        fresh = root/'fresh.zip'
        with zipfile.ZipFile(fresh, 'w') as z:
            z.writestr('fresh.txt', 'no hidden completion store')
        for i in range(2):
            report = run(['extract', fresh, '--output', root/f'fresh-{i}', '--json'], on + ['state.history=false'])
            assert report['processed_count'] == 1, report
        checks.append('history disabled: successful extraction creates no hidden dedup state')

        # Visible encrypted directories do not verify the supplied/library password.
        encrypted_zip = root/'encrypted-visible.zip'
        zip_source = root/'encrypted-source.txt'
        zip_source.write_text('verified encrypted payload')
        subprocess.run([seven, 'a', '-tzip', '-pactual-password', str(encrypted_zip), str(zip_source)], check=True, stdout=subprocess.DEVNULL)
        with sqlite3.connect(db) as connection:
            connection.execute("INSERT INTO passwords(value, source) VALUES ('wrong-library-password', 'test')")
        run(['list', encrypted_zip, '--no-empty', '--json'])
        with sqlite3.connect(db) as connection:
            assert connection.execute('SELECT COUNT(*) FROM known_files WHERE password_id IS NOT NULL').fetchone()[0] == 0
            assert connection.execute("SELECT COUNT(*) FROM file_extractions f JOIN tasks t ON f.task_id=t.id WHERE t.kind='list' AND (f.password_id IS NOT NULL OR f.has_password != 0)").fetchone()[0] == 0
        checks.append('listing a visible encrypted ZIP does not bind a wrong library password')

        legacy = root/'legacy.zip'
        raw_name = '测试文件.txt'.encode('gbk')
        placeholder = 'q' * len(raw_name)
        with zipfile.ZipFile(legacy, 'w') as z:
            z.writestr(placeholder, 'legacy contents')
        legacy.write_bytes(legacy.read_bytes().replace(placeholder.encode(), raw_name))
        previews = run(['enc', legacy, '--json'])
        gbk = next(row for row in previews if row['encoding'].lower() == 'gbk')
        assert gbk['names'] == ['测试文件.txt'], gbk
        assert len({tuple(row['names']) for row in previews if row['ok']}) > 1, previews
        run(['list', legacy, '--pick-encoding', '--non-interactive'], expected=1)
        checks.append('encoding candidates produce distinct decoded names; noninteractive picker fails explicitly')

        # PTY supplies terminal stdin while stdout remains machine-readable separately.
        data = root/'secret.txt'
        data.write_text('encrypted content')
        protected = root/'secret.7z'
        subprocess.run([seven, 'a', '-pfixture-secret', '-mhe=on', str(protected), str(data)], check=True, stdout=subprocess.DEVNULL)

        def interactive(command, replies, cancel=False):
            master, slave = pty.openpty()
            process = subprocess.Popen(base + ['--set', 'passwords.mode="manual"'] + list(map(str, command)), stdin=slave, stdout=subprocess.PIPE, stderr=slave, env=env)
            os.close(slave)
            transcript = bytearray()
            sent = 0
            deadline = time.monotonic() + 15
            try:
                while process.poll() is None and time.monotonic() < deadline:
                    if select.select([master], [], [], 0.1)[0]:
                        try:
                            chunk = os.read(master, 65536)
                        except OSError:
                            break
                        transcript.extend(chunk)
                        # English CLI password prompt ends in ': ' and does not echo secrets.
                        if b'Enter password (or press Enter to skip): ' in transcript:
                            if cancel:
                                process.send_signal(signal.SIGINT)
                                cancel = False
                                transcript.clear()
                            elif sent < len(replies):
                                os.write(master, replies[sent].encode() + b'\n')
                                sent += 1
                                transcript.clear()
                assert process.poll() is not None, ('prompt timeout', bytes(transcript))
                stdout = process.stdout.read().decode()
                return process.returncode, stdout, sent
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait()
                os.close(master)
                process.stdout.close()

        code, _, sent = interactive(['list', protected], ['wrong-first', 'fixture-secret'])
        assert code == 0 and sent == 2, (code, sent)
        code, _, _ = interactive(['list', protected], [], cancel=True)
        assert code == 130, code
        code, stdout, sent = interactive(['extract', protected, '--output', root/'interactive'], ['wrong-first', 'fixture-secret'])
        assert code == 0 and sent == 2, (code, sent, stdout)
        assert 'Password' not in stdout and 'Extracting' not in stdout, stdout
        code, _, sent = interactive(['test', protected, '--diagnose', 'off'], ['wrong-first', 'fixture-secret'])
        assert code == 0 and sent == 2, (code, sent)
        checks.append('PTY list/extract/test wrong-then-correct retry, list Ctrl+C=130, progress stays off stdout')
    print(json.dumps({'passed': len(checks), 'checks': checks}, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
