import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('install_linux', Path(__file__).with_name('install_linux.py'))
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class InstallLinuxTests(unittest.TestCase):
    def fixture(self, root):
        source = root/'package'
        (source/'bin').mkdir(parents=True)
        (source/'bin/smartzip').write_text('old CLI')
        (source/'bin/smartzip-gui').write_text('old GUI')
        (source/'release.json').write_text('{}')
        return source, root/'installed'

    def test_update_uninstall_and_user_changes(self):
        with tempfile.TemporaryDirectory() as tmp:
            source, destination = self.fixture(Path(tmp))
            installer.install(source, destination)
            (source/'bin/smartzip').write_text('new CLI')
            installer.install(source, destination)
            self.assertEqual((destination/'bin/smartzip').read_text(), 'new CLI')
            user_file = destination/'notes.txt'
            user_file.write_text('preserve')
            for operation in [lambda: installer.install(source, destination), lambda: installer.uninstall(destination)]:
                with self.assertRaises(ValueError):
                    operation()
                self.assertEqual(user_file.read_text(), 'preserve')
            user_file.unlink()
            installer.uninstall(destination)
            self.assertFalse(destination.exists())

    def test_failed_publish_restores_previous_installation(self):
        with tempfile.TemporaryDirectory() as tmp:
            source, destination = self.fixture(Path(tmp))
            installer.install(source, destination)
            (source/'bin/smartzip').write_text('new CLI')
            rename = Path.rename
            def fail_new(path, target):
                if path.name == 'new':
                    raise OSError('simulated publish failure')
                return rename(path, target)
            with patch.object(Path, 'rename', fail_new), self.assertRaises(OSError):
                installer.install(source, destination)
            self.assertEqual((destination/'bin/smartzip').read_text(), 'old CLI')

    def test_unknown_directory_and_symlink_are_preserved(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source, destination = self.fixture(root)
            destination.mkdir()
            (destination/'notes').write_text('user data')
            with self.assertRaises(ValueError):
                installer.install(source, destination)
            link = root/'link'
            link.symlink_to(destination, target_is_directory=True)
            with self.assertRaises(ValueError):
                installer.uninstall(link)
            self.assertEqual((destination/'notes').read_text(), 'user data')
