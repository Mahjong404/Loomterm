"""Controller parser and fail-closed host guards; these do not certify native PTY."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


controller = module('unix_native', Path(__file__).with_name('unix-native.py'))
runner = module('unix_runner', Path(__file__).parents[1] / 'test-unix-native.py')


class Guards(unittest.TestCase):
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
