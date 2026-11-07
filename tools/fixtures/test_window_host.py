"""Regression checks for exact-source host preparation (no native claims)."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "prepare_window_host", Path(__file__).resolve().parents[1] / "prepare-window-host.py"
)
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)


class HostPreparationTests(unittest.TestCase):
    def test_wrong_source_is_rejected_without_mutation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for name in adapter.FILES:
                p = root / name
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text("wrong revision", encoding="utf-8")
            with self.assertRaises(ValueError):
                adapter.prepare(root)
            for name in adapter.FILES:
                self.assertEqual((root / name).read_text(), "wrong revision")

    def test_paths_fail_closed_and_keep_native_environment(self):
        source = "\n".join(
            f"pub(crate) fn compute_{kind}_dir() -> anyhow::Result<PathBuf> {{\n    old()\n}}\n"
            for kind in ("cache", "data", "runtime")
        )
        result = adapter.adapted("config/src/config.rs", source)
        self.assertNotIn("old()", result)
        self.assertEqual(result.count("root.is_absolute()"), 3)
        self.assertNotIn("set_var", result)
        for kind in ("cache", "data", "runtime"):
            self.assertIn(f'root.join("{kind}")', result)


if __name__ == "__main__":
    unittest.main()
