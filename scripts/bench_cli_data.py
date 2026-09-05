#!/usr/bin/env python3
"""Fixed-size CLI data operations; wall time includes startup, excludes fixture setup."""
import argparse
import hashlib
import platform
import json
from pathlib import Path
import sqlite3
import statistics
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser()
parser.add_argument('binary', type=Path)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--samples', type=int, default=7)
args = parser.parse_args()
binary = args.binary.resolve()
results = {'binary': str(binary), 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
           'platform': platform.platform(), 'python_sqlite_version': sqlite3.sqlite_version, 'samples': args.samples, 'import_rows': 5000, 'rank_rows': 100000,
           'cleanup_rows': 5000, 'timings_seconds': {}, 'scope': 'temporary-directory filesystem; synthetic passwords; CLI startup included; setup excluded'}
with tempfile.TemporaryDirectory(prefix='smartzip-data-bench-') as work:
    root = Path(work)
    config = root / 'config.toml'
    config.write_text('[backends]\nauto_discover = false\n')
    source = root / 'passwords.txt'
    source.write_text(''.join(f'password-{n:08d}\n' for n in range(5000)))
    def command(db, *rest):
        return [str(binary), '--config', str(config), '--db', str(db), 'password', *rest]
    def call(db, *rest):
        start = time.perf_counter()
        p = subprocess.run(command(db, *rest), capture_output=True, check=True)
        return time.perf_counter() - start, p.stdout
    for operation in ['import', 'rank', 'cleanup']:
        samples = []
        for n in range(args.samples):
            db = root / f'{operation}-{n}.db'
            call(db, 'list', '--json')
            if operation != 'import':
                count = results['rank_rows' if operation == 'rank' else 'cleanup_rows']
                with sqlite3.connect(db) as c:
                    c.executemany('INSERT INTO passwords(value,source) VALUES (?,?)', ((f'password-{i:08d}', 'import') for i in range(count)))
            if operation == 'import':
                elapsed, _ = call(db, 'import', str(source))
                with sqlite3.connect(db) as c:
                    assert c.execute('SELECT count(*) FROM passwords').fetchone()[0] == 5000
            elif operation == 'rank':
                elapsed, out = call(db, 'list', '--limit', '128', '--json')
                rows = json.loads(out)
                assert len(rows) == 128 and rows[0]['value'] == 'password-00000000'
                if n == 0:
                    with sqlite3.connect(db) as c:
                        results['query_plan'] = list(c.execute("EXPLAIN QUERY PLAN SELECT id,value,source,pinned,disabled,success_count,failure_count,last_success_at,last_failure_at FROM passwords WHERE disabled=0 ORDER BY pinned DESC,success_count DESC,COALESCE(last_success_at,'') DESC,failure_count ASC,id ASC LIMIT 128"))
            else:
                elapsed, _ = call(db, 'cleanup', '--max-passwords', '128', '--stale-days', '0', '--apply')
                with sqlite3.connect(db) as c:
                    assert c.execute('SELECT count(*) FROM passwords WHERE disabled=1').fetchone()[0] == 5000
            samples.append(elapsed)
        results['timings_seconds'][operation] = {'raw': samples, 'median': statistics.median(samples)}
args.output.parent.mkdir(parents=True, exist_ok=True)
args.output.write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps(results, indent=2))
