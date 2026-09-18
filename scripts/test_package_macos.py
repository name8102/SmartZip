import importlib.util
from pathlib import Path
import plistlib
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("package_macos", Path(__file__).with_name("package-macos.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

class PackageTests(unittest.TestCase):
    def test_bundle_handles_documents_and_authenticated_finder_scheme(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            binary = root / "source"
            binary.write_bytes(b"test executable")
            target = root / "SmartZip.app"
            module.package(binary, target, sign=False)
            with (target / "Contents/Info.plist").open("rb") as file:
                info = plistlib.load(file)
            self.assertEqual(info["CFBundleIdentifier"], "org.smartzip.SmartZip")
            self.assertEqual(info["CFBundleURLTypes"][0]["CFBundleURLSchemes"], ["smartzip"])
            self.assertEqual({ext for item in info["CFBundleDocumentTypes"] for ext in item["CFBundleTypeExtensions"]}, {"zip", "7z", "rar", "tar", "gz", "bz2", "xz", "zst"})
            self.assertTrue(all(item["LSHandlerRank"] == "Alternate" for item in info["CFBundleDocumentTypes"]))
            executable = target / "Contents/MacOS/smartzip-gui"
            self.assertFalse(executable.is_symlink())
            self.assertEqual(executable.read_bytes(), binary.read_bytes())
            binary.write_bytes(b"new version")
            module.package(binary, target, sign=False)
            self.assertEqual(executable.read_bytes(), b"new version")

    def test_unknown_existing_directory_is_preserved(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            binary = root / "source"
            binary.write_bytes(b"test")
            target = root / "SmartZip.app"
            target.mkdir()
            (target / "user-file").write_text("preserve")
            with self.assertRaises(ValueError):
                module.package(binary, target, sign=False)
            self.assertEqual((target / "user-file").read_text(), "preserve")

if __name__ == "__main__":
    unittest.main()
