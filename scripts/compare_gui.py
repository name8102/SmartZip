#!/usr/bin/env python3
"""Launch the original GUI and Liquid Glass prototype with separate demo state."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path,
                        help='Path to a built smartzip-gui executable')
    parser.add_argument('--theme', choices=['light', 'dark', 'system'], default='light')
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix='smartzip-compare-'))
    executable = root / binary.name
    # Keep running instances independent of subsequent development builds.
    shutil.copy2(binary, executable)
    instances = []
    for style in ['original', 'liquid-glass']:
        directory = root / style
        directory.mkdir()
        config = directory / 'config.toml'
        config.write_text('[state]\ndatabase = '
                          + json.dumps(str(directory / 'state.sqlite3')) + '\n', encoding='utf-8')
        (directory / 'desktop.json').write_text(json.dumps({
            'color_mode': args.theme,
            'notifications': False,
            'full_window': {'x': 0, 'y': 0, 'width': 1180, 'height': 780},
        }), encoding='utf-8')
        environment = dict(os.environ, SMARTZIP_CONFIG=str(config), SMARTZIP_GUI_STYLE=style)
        log = directory / 'gui.log'
        with log.open('wb') as output:
            process = subprocess.Popen([str(executable)], env=environment,
                                       stdout=output, stderr=subprocess.STDOUT,
                                       start_new_session=True)
        instances.append({'style': style, 'pid': process.pid, 'log': str(log)})
    manifest = root / 'instances.json'
    manifest.write_text(json.dumps(instances, indent=2) + '\n', encoding='utf-8')
    print(f'Comparison state: {root}')
    print(json.dumps(instances, indent=2))


if __name__ == '__main__':
    main()
