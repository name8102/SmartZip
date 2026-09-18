import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("install_macos", Path(__file__).with_name("install-macos.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

class InstallTests(unittest.TestCase):
    def fixture(self, root):
        binary = root / "binary"
        binary.write_bytes(b"old app")
        source = root / "package/SmartZip.app"
        module.PACKAGER.package(binary, source, sign=False)
        return source

    def test_install_and_update_in_path_with_spaces(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            source = self.fixture(root)
            destination = root / "User Applications/SmartZip.app"
            module.install_bundle(source, destination, verify=False)
            binary = Path("Contents/MacOS/smartzip-gui")
            self.assertEqual((destination / binary).read_bytes(), b"old app")
            (source / binary).write_bytes(b"new app")
            module.install_bundle(source, destination, verify=False)
            self.assertEqual((destination / binary).read_bytes(), b"new app")
            self.assertTrue(source.exists())

    def test_unknown_target_is_preserved_and_failed_swap_restores_old_app(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            source = self.fixture(root)
            unknown = root / "Other.app"
            unknown.mkdir()
            (unknown / "user-file").write_text("preserve")
            with self.assertRaises(ValueError):
                module.install_bundle(source, unknown, verify=False)
            self.assertEqual((unknown / "user-file").read_text(), "preserve")
            destination = root / "Applications/SmartZip.app"
            module.install_bundle(source, destination, verify=False)
            original_rename = Path.rename
            def fail_publish(path, target):
                if path.name == "SmartZip.app" and path.parent.name.startswith(".smartzip-install-"):
                    raise OSError("simulated failed publish")
                return original_rename(path, target)
            with patch.object(Path, "rename", fail_publish):
                with self.assertRaises(OSError):
                    module.install_bundle(source, destination, verify=False)
            self.assertEqual((destination / "Contents/MacOS/smartzip-gui").read_bytes(), b"old app")

if __name__ == "__main__":
    unittest.main()
