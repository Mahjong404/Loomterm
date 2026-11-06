"""Controller parser and fail-closed host guards; these do not certify native PTY."""
import importlib.util
from pathlib import Path
import unittest
import tempfile
from types import SimpleNamespace
from unittest.mock import patch


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


controller = module('unix_native', Path(__file__).with_name('unix-native.py'))
runner = module('unix_runner', Path(__file__).parents[1] / 'test-unix-native.py')


class Guards(unittest.TestCase):
    def test_case_uses_short_tmp_root_instead_of_macos_tmpdir(self):
        with tempfile.TemporaryDirectory(prefix='lt-root-policy-') as directory:
            base = Path(directory) / 'short'
            base.mkdir()
            args = SimpleNamespace(host=Path('host'), exe=Path('exe'), fixture=Path('fixture'),
                                   evidence=Path(directory) / 'evidence')
            with patch.object(controller.tempfile, 'mkdtemp', return_value=str(base)) as factory:
                case = controller.Case(args, 'preflight')
            factory.assert_called_once_with(prefix='lt-m2-', dir='/tmp')
            self.assertEqual(case.root.parent, base.resolve())
        endpoint = Path('/private/tmp/lt-m2-abcdefgh/product/runtime/m2-12345678/loomterm-client.sock')
        self.assertLessEqual(len(str(endpoint).encode('utf-8')), 103)

    def test_failed_tool_exports_phase_and_diagnostic_to_ci_summary(self):
        with tempfile.TemporaryDirectory(prefix='lt-failed-tool-') as directory:
            out = Path(directory) / 'evidence'
            def failing_tool(_argv, **kwargs):
                kwargs['stdout'].write(b'controlled prerequisite failure\n')
                return SimpleNamespace(pid=123, wait=lambda _timeout: 1)
            with patch.object(runner, 'target', return_value='x86_64-unknown-linux-gnu'), patch.object(runner.shutil, 'which', return_value='synthetic-tool'), patch.object(runner.subprocess, 'Popen', side_effect=failing_tool), patch.object(runner.sys, 'argv', ['native-test', '--evidence', str(out)]), patch.dict(runner.os.environ), patch('builtins.print'):
                runner.os.environ.pop('LOOMTERM_NATIVE_MANIFEST', None)
                self.assertEqual(runner.main(), 1)
            import json
            summary = json.loads((out / 'ci-summary.json').read_text())
            self.assertEqual(summary['status'], 'Fail')
            self.assertEqual(summary['active_phase'], 'version-rustc')
            self.assertIn('exited 1', summary['error'])
            self.assertIn('controlled prerequisite failure', summary['diagnostic_tail'])
            self.assertEqual(summary['completed_phases'], [])

    def test_failure_diagnostic_is_bounded_and_redacts_paths(self):
        with tempfile.TemporaryDirectory(prefix='lt-diagnostic-') as directory:
            root = Path(directory)
            repo, evidence = root / 'source', root / 'evidence'
            log = root / 'compiler.log'
            log.write_text('x' * 20000 + '\n' + str(repo / 'src/main.rs') + '\n' + str(evidence / 'target') + '\n' + str(Path.home() / '.cache'), encoding='utf-8')
            text = runner.diagnostic_tail(log, repo, evidence)
            self.assertLessEqual(len(text.encode('utf-8')), 8192)
            for path in (repo, evidence, Path.home()):
                self.assertNotIn(str(path), text)
            self.assertIn('<source>', text)
            self.assertIn('<evidence>', text)
            self.assertIn('<runner-home>', text)

    def test_wrong_version_requires_an_actual_welcome_with_error(self):
        reason = b'protocol version mismatch'
        welcome = bytes([0, 1, 0, 1, len(reason)]) + reason
        self.assertEqual(controller.rejection(welcome)['reason'], reason.decode())
        for invalid in (bytes([1]) + welcome[1:], welcome + b'x', bytes([0, 1, 0, 0])):
            with self.assertRaises((AssertionError, IndexError)):
                controller.rejection(invalid)

    def test_fragmented_frame_and_eof(self):
        class Stream:
            def __init__(self):
                self.blocks = iter([b'a', b'bc', b''])
            def recv(self, _length):
                return next(self.blocks)
        stream = Stream()
        self.assertEqual(controller.exact(stream, 3), b'abc')
        with self.assertRaises(EOFError):
            controller.exact(stream, 1)

    def test_windows_and_macos_intel_cannot_claim_target_native(self):
        for system, machine in [('Windows', 'AMD64'), ('Darwin', 'x86_64'), ('Linux', 'aarch64')]:
            with patch.object(runner.platform, 'system', return_value=system), patch.object(runner.platform, 'machine', return_value=machine):
                with self.assertRaises(RuntimeError):
                    runner.target()

    def test_linux_musl_is_not_glibc_acceptance(self):
        with patch.object(runner.platform, 'system', return_value='Linux'), patch.object(runner.platform, 'machine', return_value='x86_64'), patch.object(runner.platform, 'libc_ver', return_value=('musl', '1.2')):
            with self.assertRaises(RuntimeError):
                runner.target()


if __name__ == '__main__':
    unittest.main()
