import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import package_native_runtime as runtime


def fixture_runtime(directory, platform='linux'):
    directory = Path(directory)
    (directory / 'smartzip-archive-worker').write_bytes(b'worker')
    library = directory / 'lib' / runtime.LIBRARY_NAMES[platform]
    library.parent.mkdir(parents=True, exist_ok=True)
    library.write_bytes(b'library')
    (library.parent / '7zip-source.tar.xz').write_bytes(b'source')
    licenses = library.parent / 'licenses/7zip'
    licenses.mkdir(parents=True, exist_ok=True)
    for name in ('License.txt', 'copying.txt', 'unRarLicense.txt', 'source.json'):
        (licenses / name).write_text('{"version":"26.06"}' if name == 'source.json' else name)
    return library


class NativeRuntimeTests(unittest.TestCase):
    def setUp(self):
        environment = patch.dict(runtime.os.environ, {'SMARTZIP_7Z_LIBRARY': '', 'SMARTZIP_ARCHIVE_WORKER': ''})
        environment.start()
        self.addCleanup(environment.stop)

    def test_package_contains_worker_library_licenses_and_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture_runtime(root)
            target = root / 'package/bin'
            runtime.copy_runtime(root / 'smartzip', target, platform='linux')
            self.assertEqual((target / 'smartzip-archive-worker').read_bytes(), b'worker')
            self.assertEqual((target / 'lib/7z.so').read_bytes(), b'library')
            self.assertEqual((target / 'lib/7zip-source.tar.xz').read_bytes(), b'source')
            self.assertTrue((target / 'lib/licenses/7zip/unRarLicense.txt').is_file())

    def test_custom_worker_is_installed_under_discoverable_name(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture_runtime(root)
            custom = root / 'custom-worker'
            custom.write_bytes(b'custom worker')
            target = root / 'package'
            runtime.copy_runtime(root / 'smartzip', target, worker=custom, platform='linux')
            self.assertEqual((target / 'smartzip-archive-worker').read_bytes(), b'custom worker')

    def test_missing_material_fails_before_copy(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            library = fixture_runtime(root)
            library.unlink()
            target = root / 'package'
            with self.assertRaisesRegex(ValueError, 'Missing native runtime material'):
                runtime.copy_runtime(root / 'smartzip', target, platform='linux')
            self.assertFalse(target.exists())

    def test_source_hash_is_checked_before_extracting_or_building(self):
        spec = importlib.util.spec_from_file_location('package_sevenzip', Path(__file__).with_name('package_sevenzip.py'))
        builder = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(builder)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / 'source.tar.xz'
            archive.write_bytes(b'incorrect source')
            with self.assertRaisesRegex(ValueError, 'SHA-256 mismatch'):
                builder.build(root / 'lib', archive)
            self.assertFalse((root / 'lib').exists())


if __name__ == '__main__':
    unittest.main()
