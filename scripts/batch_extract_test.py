#!/usr/bin/env python3
"""Extract directory inputs, then diagnose failures. Requires Python 3.9+.

Usage: python3 scripts/batch_extract_test.py SOURCE DEST --ask-password
See scripts/batch_extract_test.md for report semantics and options.
"""
import argparse
import getpass
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
from datetime import datetime


def chunks(paths, limit=48000):
    batch, size = [], 0
    for path in paths:
        length = len(os.fsencode(path)) + 1
        if batch and size + length > limit:
            yield batch
            batch, size = [], 0
        batch.append(str(path))
        size += length
    if batch:
        yield batch


def write_json(path, value):
    temporary = path.with_suffix(path.suffix + '.tmp')
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    temporary.replace(path)


def redact(value, passwords):
    if isinstance(value, str):
        for password in passwords:
            if password:
                value = value.replace(password, '<redacted>')
        return value
    if isinstance(value, list):
        return [redact(v, passwords) for v in value]
    if isinstance(value, dict):
        return {k: redact(v, passwords) for k, v in value.items()}
    return value


def run_json(base, args, prefix, passwords):
    # Never persist argv; redact supplied secrets from backend diagnostics too.
    proc = subprocess.run(base + args, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    stdout = proc.stdout.decode('utf-8', errors='replace')
    stderr = proc.stderr.decode('utf-8', errors='replace')
    try:
        data = json.loads(stdout)
    except ValueError:
        data = None

    write_json(prefix.with_suffix('.json'), redact(data, passwords))
    prefix.with_suffix('.stderr.txt').write_text(redact(stderr, passwords), encoding='utf-8')
    if data is None:
        prefix.with_suffix('.stdout.txt').write_text(redact(stdout, passwords), encoding='utf-8')
    if proc.returncode in (130, -2):
        raise KeyboardInterrupt
    return proc.returncode, data


def summary_text(report):
    lines = [
        f"输入文件：{report['input_count']}",
        f"解压失败记录：{len(report['failed'])}",
        f"已跳过记录：{len(report['skipped'])}（跳过不等于损坏）",
        f"校验归档组：{len(report['tests'])}",
        '', '确认损坏的物理文件/分卷：',
    ]
    confirmed = sorted({v['path'] for t in report['tests'] for v in t.get('confirmed_volumes', [])})
    lines.extend(confirmed or ['无确认项（不代表全部完好）'])
    lines.extend(['', '解压失败输入与原因（失败不等于损坏）：'])
    lines.extend(f"{row['input_path']}：{row.get('reason') or 'unknown'}" for row in report['failed'])
    for t in report['tests']:
        lines.extend(['', f"归档：{t['entrypoint']}",
                      f"完整性={t['integrity']} 覆盖={t['coverage']} 密码={t['password_status']}"])
        for group in t.get('suspect_groups', []):
            lines.append('疑似组（不能认定每卷都坏）：' + json.dumps(group['members'], ensure_ascii=False))
        for key, label in [('damaged_files', '后端报告的内部损坏条目'),
                           ('missing_volumes', '缺失'), ('unreadable_volumes', '不可读'),
                           ('unchecked_volumes', '未检查'), ('stop_reasons', '诊断说明')]:
            for item in t.get(key, []):
                lines.append(f'  {label}：{item}')
    if report['issues']:
        lines.extend(['', '未解决的问题（报告可能不完整）：', *report['issues']])
    lines.extend(['', '各文件失败/跳过原因及完整证据见 report.json 和分阶段 JSON。'])
    return '\n'.join(lines) + '\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('destination', type=Path)
    parser.add_argument('--recursive', action='store_true', help='包含源目录的子目录；不递归解压包内归档')
    parser.add_argument('--ask-password', action='count', default=0, help='隐藏输入候选密码，可重复指定')
    parser.add_argument('--smartzip', default='smartzip', help='smartzip 可执行文件路径')
    parser.add_argument('--report-dir', type=Path, help='必须是尚不存在的目录')
    parser.add_argument('--diagnostic-timeout', type=int, default=60, help='每组追加诊断秒数；不是完整校验时限')
    args = parser.parse_args()
    source, destination = args.source.resolve(), args.destination.resolve()
    if not source.is_dir():
        parser.error('源目录不存在')
    if destination == source or source in destination.parents or destination in source.parents:
        parser.error('源目录和目标目录必须互不包含')
    binary = shutil.which(args.smartzip)
    if not binary:
        parser.error('找不到 smartzip；请用 --smartzip 指定路径')
    if args.diagnostic_timeout < 1:
        parser.error('--diagnostic-timeout 必须大于零')
    report_dir = (args.report_dir or destination / ('smartzip-report-' + datetime.now().strftime('%Y%m%d-%H%M%S-%f'))).resolve()
    if report_dir == source or source in report_dir.parents:
        parser.error('报告目录不能放在源目录内')
    paths = []
    # Fail visibly on unreadable directories instead of silently omitting inputs.
    def walk_error(error):
        raise error
    for root, directories, files in os.walk(source, onerror=walk_error, followlinks=False):
        directories[:] = sorted(d for d in directories if not (Path(root) / d).is_symlink()) if args.recursive else []
        paths.extend(Path(root) / name for name in sorted(files)
                     if not (Path(root) / name).is_symlink() and (Path(root) / name).is_file())
    passwords = [getpass.getpass(f'候选密码 {i + 1}：') for i in range(args.ask_password)]
    password_args = [item for password in passwords for item in ('-p', password)]
    destination.mkdir(parents=True, exist_ok=True)
    report_dir.mkdir(parents=True, mode=0o700, exist_ok=False)
    base = [binary, '--no-config']
    report = dict(input_count=len(paths), failed=[], skipped=[], tests=[], issues=[])

    def save():
        safe_report = redact(report, passwords)
        write_json(report_dir / 'report.json', safe_report)
        (report_dir / 'summary.txt').write_text(summary_text(safe_report), encoding='utf-8')

    try:
        for index, batch in enumerate(chunks(paths), 1):
            print(f'解压批次 {index}：{len(batch)} 个输入', flush=True)
            code, result = run_json(base, ['extract', *batch, '-o', str(destination),
                '--json', '--non-interactive', '--no-recursive', '--force',
                '--on-conflict', 'rename', *password_args], report_dir / f'extract-{index}', passwords)
            if not isinstance(result, dict) or not result.get('task_id'):
                report['issues'].append(f'批次 {index} 无可用任务结果，退出码 {code}；见 extract-{index}.json')
                save()
                continue
            history_code, history = run_json(base, ['history', 'show', result['task_id'], '--json'],
                report_dir / f'history-{index}', passwords)
            if history_code != 0 or not isinstance(history, dict) or not isinstance(history.get('files'), list):
                report['issues'].append(f'批次 {index} 无逐文件历史，不能可靠筛选失败文件')
                save()
                continue
            failed = [row for row in history['files'] if row['status'] == 'failed']
            report['failed'].extend(failed)
            report['skipped'].extend(row for row in history['files'] if row['status'] == 'skipped')
            if len(failed) < result.get('failed_count', 0) or code not in (0, 1, 2):
                report['issues'].append(f'批次 {index} 失败计数或退出码异常，请检查原始报告')
            save()

        # Let SmartZip resolve/deduplicate volume groups, including arbitrary member inputs.
        failed_paths = sorted({row['input_path'] for row in report['failed']})
        for index, batch in enumerate(chunks(failed_paths), 1):
            print(f'校验失败输入，批次 {index}：{len(batch)} 个', flush=True)
            code, result = run_json(base, ['test', *batch, '--json', '--diagnose', 'auto',
                '--diagnostic-timeout', str(args.diagnostic_timeout), *password_args],
                report_dir / f'test-{index}', passwords)
            if isinstance(result, dict) and isinstance(result.get('files'), list):
                report['tests'].extend(result['files'])
                covered = {p for t in result['files'] for p in t.get('input_paths', [])}
                if set(batch) - covered:
                    report['issues'].append(f'校验批次 {index} 有输入未出现在报告中')
                if code not in (0, 1, 2):
                    report['issues'].append(f'校验批次 {index} 退出码异常：{code}')
            else:
                report['issues'].append(f'校验批次 {index} 无有效报告，退出码 {code}')
            save()
    except KeyboardInterrupt:
        report['issues'].append('用户中断，未完成全部处理')
        save()
        print(f'已保存部分报告：{report_dir}')
        return 130
    save()
    print(summary_text(redact(report, passwords)))
    print(f'报告目录：{report_dir}')
    return 2 if report['issues'] else 1 if report['failed'] or report['skipped'] else 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, ValueError) as error:
        print(f'脚本错误：{error}', file=sys.stderr)
        sys.exit(2)
