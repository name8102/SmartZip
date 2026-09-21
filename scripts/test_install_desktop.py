import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location('install_desktop', Path(__file__).with_name('install_desktop.py'))
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class InstallDesktopTests(unittest.TestCase):
    def test_build_uses_cargo_artifacts_in_custom_target_directory(self):
        binaries = {'smartzip': '/custom target/native/debug/smartzip',
                    'smartzip-gui': '/custom target/native/debug/smartzip-gui'}
        output = '\n'.join(json.dumps({'reason': 'compiler-artifact', 'target': {'name': name},
                                       'executable': path}) for name, path in binaries.items())
        with patch.object(installer.subprocess, 'run', return_value=Mock(stdout=output)) as run:
            self.assertEqual(installer.build('debug'), {k: Path(v) for k, v in binaries.items()})
            self.assertNotIn('--release', run.call_args.args[0])

    def test_native_platform_selection_and_cli_install(self):
        binaries = {'smartzip': Path('/build/smartzip'), 'smartzip-gui': Path('/build/smartzip-gui')}
        for platform in ('linux', 'darwin'):
            with self.subTest(platform=platform), patch.object(installer.sys, 'platform', platform), \
                    patch.object(installer, 'build', return_value=binaries), \
                    patch.object(installer, 'install_linux') as linux, \
                    patch.object(installer, 'load') as load, \
                    patch.object(installer.subprocess, 'run') as run:
                installer.main(['--profile', 'debug', '--destination', '/custom/SmartZip.app'])
                if platform == 'linux':
                    linux.assert_called_once_with(binaries, Path('/custom/SmartZip.app'))
                    load.assert_not_called()
                else:
                    load.return_value.install_bundle.assert_called_once()
                    linux.assert_not_called()
                self.assertIn('--debug', run.call_args.args[0])
                self.assertIn('crates/smartzip-cli', run.call_args.args[0])

    def test_gui_only_does_not_install_cli(self):
        with patch.object(installer.sys, 'platform', 'linux'), \
                patch.object(installer, 'build'), patch.object(installer, 'install_linux'), \
                patch.object(installer.subprocess, 'run') as run:
            installer.main(['--gui-only'])
            run.assert_not_called()

    def test_linux_package_update_and_launcher(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binaries = {name: root / name for name in ('smartzip', 'smartzip-gui')}
            for binary in binaries.values():
                binary.write_text('binary')
            destination = root / 'User Apps/SmartZip'
            with patch.dict(installer.os.environ, {'XDG_DATA_HOME': str(root / 'data')}), \
                    patch.object(installer.subprocess, 'check_output', return_value='smartzip 0.1.0\n'):
                bundle = installer.install_linux(binaries, destination)
                binaries['smartzip-gui'].write_text('updated')
                installer.install_linux(binaries, destination)
            self.assertEqual((destination / 'bin/smartzip-gui').read_text(), 'updated')
            self.assertTrue((bundle / 'release.json').is_file())
            launcher = (root / 'data/applications/org.smartzip.SmartZip.desktop').read_text()
            self.assertIn(f'Exec="{destination}/bin/smartzip-gui" %F', launcher)

    def test_unsupported_platform_fails_before_build(self):
        with patch.object(installer.sys, 'platform', 'win32'), patch.object(installer, 'build') as build:
            with self.assertRaises(SystemExit):
                installer.main([])
            build.assert_not_called()


if __name__ == '__main__':
    unittest.main()
