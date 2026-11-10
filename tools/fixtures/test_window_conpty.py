"""Fail-closed staging checks; optional real fixed input, never downloads."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "stage_window_conpty", Path(__file__).resolve().parents[1] / "stage-window-conpty.py"
)
stager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stager)


class ConPtyStagingTests(unittest.TestCase):
    def test_unknown_package_leaves_existing_output_untouched(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package = root / "wrong.nupkg"
            package.write_bytes(b"untrusted package")
            output = root / "output"
            output.mkdir()
            (output / "sentinel").write_bytes(b"existing consumer")
            with self.assertRaises(ValueError):
                stager.stage(package, output)
            self.assertEqual([p.name for p in output.iterdir()], ["sentinel"])
            self.assertEqual((output / "sentinel").read_bytes(), b"existing consumer")

    @unittest.skipUnless(os.environ.get("WINDOW_CONPTY_PACKAGE"), "fixed package not supplied")
    def test_real_package_is_idempotent_and_rejects_bundle_replacement(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "stage"
            package = Path(os.environ["WINDOW_CONPTY_PACKAGE"])
            stager.stage(package, output)
            before = {p.relative_to(output): p.read_bytes()
                      for p in output.rglob("*") if p.is_file()}
            stager.stage(package, output)
            self.assertEqual(before, {p.relative_to(output): p.read_bytes()
                                      for p in output.rglob("*") if p.is_file()})
            legal = output / "notices/Microsoft.Windows.Console.ConPTY/NOTICE.md"
            legal.write_bytes(b"different legal notice")
            with self.assertRaises(ValueError):
                stager.stage(package, output)
            self.assertEqual(legal.read_bytes(), b"different legal notice")
            legal.write_bytes(before[legal.relative_to(output)])
            native = output / "conpty/conpty.dll"
            native.write_bytes(b"unknown existing consumer")
            with self.assertRaises(ValueError):
                stager.stage(package, output)
            self.assertEqual(native.read_bytes(), b"unknown existing consumer")


if __name__ == "__main__":
    unittest.main()
