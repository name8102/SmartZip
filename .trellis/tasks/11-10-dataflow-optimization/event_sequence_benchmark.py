"""Bounded SQLite hot-path microbenchmark; not archive end-to-end timing.
Run: python3 event_sequence_benchmark.py
"""
import json
import platform
import sqlite3
import time

ROWS = 5000
REPEATS = 3
results = {}
for mode in ('original', 'indexed_max', 'single_id'):
    samples = []
    for _ in range(REPEATS):
        conn = sqlite3.connect(':memory:')
        conn.executescript('''CREATE TABLE task_events (
          id INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT, sequence INTEGER,
          created_at TEXT DEFAULT CURRENT_TIMESTAMP);
          CREATE INDEX idx_task_events_task ON task_events(task_id,created_at);
          CREATE UNIQUE INDEX idx_task_events_sequence
          ON task_events(task_id,sequence) WHERE sequence IS NOT NULL;''')
        if mode == 'single_id':
            conn.executescript('''DROP INDEX idx_task_events_sequence;
              DROP INDEX idx_task_events_task;
              CREATE INDEX idx_task_events_task ON task_events(task_id);''')
            sql = 'INSERT INTO task_events(task_id) VALUES (?)'
            parameters = ('task',)
        else:
            predicate = ' AND sequence IS NOT NULL' if mode == 'indexed_max' else ''
            sql = ('INSERT INTO task_events(task_id,sequence) VALUES '
                   '(?,(SELECT COALESCE(MAX(sequence),0)+1 FROM task_events '
                   'WHERE task_id=?' + predicate + '))')
            parameters = ('task', 'task')
        start = time.perf_counter()
        with conn:
            for _ in range(ROWS):
                conn.execute(sql, parameters)
        samples.append(time.perf_counter() - start)
        assert conn.execute('SELECT COUNT(*) FROM task_events').fetchone()[0] == ROWS
        conn.close()
    results[mode] = samples
print(json.dumps({'rows': ROWS, 'repeats': REPEATS, 'sqlite': sqlite3.sqlite_version,
                  'python': platform.python_version(), 'seconds': results}, indent=2))
