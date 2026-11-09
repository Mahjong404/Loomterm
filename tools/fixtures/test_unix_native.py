"""Controller parser and fail-closed host guards; these do not certify native PTY."""
import importlib.util
from collections import namedtuple
from pathlib import Path
import unittest
import tempfile
import json
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
    def test_lint_configuration_rejects_missing_changed_or_foreign_config(self):
        import hashlib
        with tempfile.TemporaryDirectory(prefix='lt-config-') as directory:
            app = Path(directory)
            content = b'too-many-arguments-threshold = 11\n'
            expected = hashlib.sha256(content).hexdigest()
            (app / 'UPSTREAM.json').write_text(json.dumps(dict(files=[dict(path='clippy.toml', sha256=expected)])))
            config = app / 'clippy.toml'
            with self.assertRaisesRegex(RuntimeError, 'source delivery'):
                runner.lint_configuration(app, {})
            config.write_bytes(content.replace(b'11', b'7'))
            with self.assertRaisesRegex(RuntimeError, 'source delivery'):
                runner.lint_configuration(app, {})
            config.write_bytes(content)
            self.assertEqual(runner.lint_configuration(app, {})['sha256'], expected)
            with self.assertRaisesRegex(RuntimeError, 'outside'):
                runner.lint_configuration(app, {'CLIPPY_CONF_DIR': str(app / 'foreign')})

    def test_threshold_probe_requires_both_positive_and_negative_control(self):
        with tempfile.TemporaryDirectory(prefix='lt-threshold-') as directory:
            app = Path(directory)
            calls = []
            def driver(argv, **kwargs):
                self.assertEqual(kwargs['cwd'], app)
                self.assertEqual(argv[:4], ['rustup', 'run', '1.96.1', 'clippy-driver'])
                count = Path(argv[4]).read_text().count(': u8')
                calls.append(count)
                if count == 12:
                    kwargs['stdout'].write(json.dumps(dict(level='error', code=dict(code='clippy::too_many_arguments'),
                        message='this function has too many arguments (12/11)', spans=[{}])).encode())
                return SimpleNamespace(returncode=0 if count == 11 else 1)
            with patch.object(runner.subprocess, 'run', side_effect=driver):
                self.assertEqual(runner.clippy_threshold_probe(app, app, {})['threshold'], 11)
            self.assertEqual(calls, [11, 12])
            with patch.object(runner.subprocess, 'run', return_value=SimpleNamespace(returncode=0)):
                with self.assertRaisesRegex(RuntimeError, 'threshold'):
                    runner.clippy_threshold_probe(app, app, {})

    def test_clippy_export_deduplicates_targets_without_source_or_rendered_text(self):
        with tempfile.TemporaryDirectory(prefix='lt-clippy-json-') as directory:
            app = Path(directory) / 'app'
            (app / 'src').mkdir(parents=True)
            (app / 'src/main.rs').write_text('fn inherited() {}\n')
            message = dict(reason='compiler-message', manifest_path=str(app / 'Cargo.toml'),
                           target=dict(name='loomterm', kind=['bin']),
                           message=dict(level='error', code=dict(code='clippy::too_many_arguments'),
                                        message='this function has too many arguments (8/7)',
                                        rendered='PRIVATE_RENDERED',
                                        spans=[dict(file_name='src/main.rs', line_start=1, column_start=1,
                                                    is_primary=True, text=[dict(text='fn inherited(PRIVATE_SOURCE)')])]))
            log = Path(directory) / 'clippy.log'
            log.write_text(json.dumps(message) + '\n' + json.dumps(message) + '\n')
            result = runner.clippy_diagnostics(log, app)
            self.assertTrue(result['complete'])
            self.assertEqual(result['occurrences'], 2)
            self.assertEqual(len(result['records']), 1)
            record = result['records'][0]
            self.assertEqual((record['path'], record['actual_args'], record['threshold']), ('src/main.rs', 8, 7))
            self.assertEqual(record['occurrences'], 2)
            exported = json.dumps(result)
            for forbidden in (directory, 'PRIVATE_RENDERED', 'PRIVATE_SOURCE', 'manifest_path'):
                self.assertNotIn(forbidden, exported)

    def test_clippy_export_rejects_external_paths_and_marks_incomplete(self):
        with tempfile.TemporaryDirectory(prefix='lt-clippy-foreign-') as directory:
            app = Path(directory) / 'app'
            app.mkdir()
            log = Path(directory) / 'clippy.log'
            log.write_text(json.dumps(dict(reason='compiler-message', target=dict(name='loomterm', kind=['bin']),
                message=dict(level='error', code=dict(code='clippy::too_many_arguments'),
                             message='PRIVATE_MESSAGE', spans=[dict(file_name=str(Path.home() / 'private.rs'),
                             line_start=1, column_start=1, is_primary=True)]))) + '\n')
            report = runner.clippy_diagnostics(log, app)
            self.assertFalse(report['complete'])
            self.assertEqual(report['rejected'], 1)
            self.assertEqual(report['records'], [])
            self.assertNotIn('PRIVATE_MESSAGE', json.dumps(report))

    def test_clippy_export_bounds_unique_records_and_reports_truncation(self):
        with tempfile.TemporaryDirectory(prefix='lt-clippy-cap-') as directory:
            app = Path(directory) / 'app'
            (app / 'src').mkdir(parents=True)
            (app / 'src/main.rs').write_text('// synthetic compiler input\n')
            log = Path(directory) / 'clippy.log'
            lines = []
            for line in range(1, 151):
                lines.append(json.dumps(dict(reason='compiler-message', target=dict(name='loomterm', kind=['bin']),
                    message=dict(level='error', code=dict(code='clippy::too_many_arguments'),
                    message='this function has too many arguments (8/7)', spans=[dict(file_name='src/main.rs',
                    line_start=line, column_start=1, is_primary=True)]))))
            log.write_text('\n'.join(lines))
            result = runner.clippy_diagnostics(log, app)
            self.assertFalse(result['complete'])
            self.assertTrue(result['truncated'])
            self.assertEqual(len(result['records']), 128)
            self.assertLessEqual(len(json.dumps(result).encode()), 65536)

    def test_boundary_events_require_the_new_child_token(self):
        records = []
        text = '\n'.join(json.dumps(dict(event='ready', token=token, pid=pid))
                         for token, pid in [('original', 10), ('fresh', 11)])
        case = SimpleNamespace(token='original', api=lambda *_: {'read': {'text': text}},
                               record=lambda *args, **kwargs: records.append(kwargs))
        result = controller.Case.fixture_event(case, dict(pane_id='pane'), 'ready', token='fresh')
        self.assertEqual(result['pid'], 11)
        self.assertEqual(len(records), 1)
        self.assertIsNone(controller.Case.fixture_event(case, dict(pane_id='pane'), 'ready', token='absent'))

    def test_reused_pid_cannot_satisfy_a_live_child_identity(self):
        saved = dict(pid=10, birth='old', uid=1001, executable='/fixture')
        with patch.object(controller, 'identity', return_value=saved | dict(birth='new', exited=False)):
            self.assertFalse(controller.same_process(saved))

    def test_all_three_native_cases_are_required_for_success(self):
        with tempfile.TemporaryDirectory(prefix='lt-three-cases-') as directory:
            calls = []
            class Case:
                def __init__(self, _args, label):
                    self.result = dict(label=label, status='Fail')
                def finish(self):
                    pass
            def passed(case, concurrent):
                calls.append((case.result['label'], concurrent))
                case.result['status'] = 'Pass'
            argv = ['native', '--exe', 'exe', '--host', 'host', '--fixture', 'fixture', '--evidence', directory]
            with patch.object(controller, 'Case', Case), patch.object(controller, 'acceptance', side_effect=passed), patch.object(controller, 'exit_source_boundaries', side_effect=passed), patch.object(controller.sys, 'argv', argv), patch.object(controller.sys, 'platform', 'linux'), patch.object(controller.signal, 'signal'), patch('builtins.print'):
                self.assertEqual(controller.main(), 0)
            self.assertEqual(calls, [('lifecycle', False), ('cold-race', True), ('exit-source-boundaries', False)])

    def test_reattach_selects_split_and_requires_child_marker_ack(self):
        commands = []
        pane = dict(pane_id='split', tab_id='tab2')
        projected = {'ready': False, 'right': False}
        def send(command):
            commands.append(command)
            if command == '\x02l':
                projected['right'] = projected['ready']
        def visible(text):
            self.assertEqual(commands, ['\x022'])
            self.assertEqual(text, '"event":"continuation"')
            projected['ready'] = True
        case = SimpleNamespace(token='unitfixture', result={},
                               focused=lambda: pane if projected['right'] else dict(tab_id='tab2', pane_id='left'),
                               fixture_event=lambda _pane, _event, **fields: dict(value=fields['value'])
                               if commands[-1] == 'MARK unitfixturer\r' else None)
        host = SimpleNamespace(case=case, text=send, visible=visible)
        result = controller.Host.select_test_split(host, pane)
        self.assertEqual(commands, ['\x022', '\x02l', 'MARK unitfixturer\r'])
        self.assertEqual(result['value'], 'unitfixturer')

    def test_reattach_wrong_focus_cannot_send_work_input(self):
        commands = []
        case = SimpleNamespace(token='unitfixture', result={}, focused=lambda: dict(tab_id='tab2', pane_id='other'))
        host = SimpleNamespace(case=case, text=commands.append, visible=lambda _text: None)
        def once(check):
            result = check()
            if not result:
                raise TimeoutError('focus not confirmed')
            return result
        with patch.object(controller, 'wait_for', side_effect=once):
            with self.assertRaises(TimeoutError):
                controller.Host.select_test_split(host, dict(pane_id='split', tab_id='tab2'))
        self.assertEqual(commands, ['\x022', '\x02l'])

    def test_reattach_old_tab_cannot_issue_directional_command(self):
        commands = []
        case = SimpleNamespace(token='unitfixture', result={}, focused=lambda: dict(tab_id='tab1', pane_id='old'))
        host = SimpleNamespace(case=case, text=commands.append)
        def once(check):
            result = check()
            if not result:
                raise TimeoutError('tab not confirmed')
            return result
        with patch.object(controller, 'wait_for', side_effect=once):
            with self.assertRaises(TimeoutError):
                controller.Host.select_test_split(host, dict(pane_id='split', tab_id='tab2'))
        self.assertEqual(commands, ['\x022'])

    def test_runtime_identity_export_retains_boot_terminal_birth_and_drops_paths(self):
        event = dict(event='reattached', time_ns=1,
                     instance=dict(product='loomterm', namespace='test', instance_id='boot', pid=10, root='private-path'),
                     pane=dict(pane_id='pane', terminal_id='terminal', tab_id='tab', cwd='private-path'),
                     shell=dict(pid=11, birth='ticks', uid=1001, executable='private-path'),
                     fixture=dict(pid=12, birth='ticks2', uid=1001, executable='private-path'))
        public = runner.public_runtime_identity(event)
        self.assertEqual(public['backend']['instance_id'], 'boot')
        self.assertEqual(public['pane']['terminal_id'], 'terminal')
        self.assertEqual(public['shell']['birth'], 'ticks')
        self.assertNotIn('private-path', json.dumps(public))

    def test_late_lint_failure_retains_runtime_completion_but_fails_overall(self):
        with tempfile.TemporaryDirectory(prefix='lt-lint-order-') as directory:
            out = (Path(directory) / 'evidence').resolve()
            order = []
            def tool(argv, **kwargs):
                label = ' '.join(argv[:2])
                order.append(label)
                code, text = 0, ''
                if argv[0] == 'rustc':
                    text = 'rustc 1.96.1 (fixture)\n'
                elif argv[0] == 'cargo' and argv[1] == '--version':
                    text = 'cargo 1.96.1 (fixture)\n'
                elif argv[0] == 'zig':
                    text = '0.16.0\n'
                elif argv[:2] == ['cargo', 'test'] and '--message-format=json' in argv:
                    for name in ('loomterm', 'loomterm_native'):
                        file = out / name
                        file.write_bytes(b'synthetic, never executed')
                        text += json.dumps(dict(reason='compiler-artifact', target=dict(name=name), executable=str(file))) + '\n'
                    native = out / 'target/x86_64-unknown-linux-gnu/debug/build/ghostty-vt-fixture/out/zig-out/lib/libghostty-vt.a'
                    native.parent.mkdir(parents=True)
                    native.write_bytes(b'synthetic, never linked')
                elif argv[0] == 'cc' and '-o' in argv and '-###' not in argv:
                    Path(argv[argv.index('-o') + 1]).write_bytes(b'synthetic fixture, never executed')
                elif argv[:2] == ['cargo', 'clippy']:
                    code, text = 101, json.dumps(dict(reason='compiler-message',
                        message=dict(level='error', code=dict(code='clippy::too_many_arguments'),
                        message='this function has too many arguments (8/7)', rendered='PRIVATE_RENDERED',
                        spans=[dict(file_name='src/main.rs', line_start=1, column_start=1, is_primary=True)]))) + '\n'
                if 'stdout' in kwargs:
                    kwargs['stdout'].write(text.encode())
                return SimpleNamespace(pid=123, wait=lambda _timeout: code)
            with patch.object(runner, 'clippy_threshold_probe', return_value=dict(status='Pass', threshold=11)), patch.object(runner, 'target', return_value='x86_64-unknown-linux-gnu'), patch.object(runner.sys, 'platform', 'linux'), patch.object(runner.shutil, 'which', return_value='synthetic-tool'), patch.object(runner.platform, 'uname', return_value=namedtuple('Uname', 'system node release version machine processor')('Linux', 'fixture', 'kernel', 'version', 'x86_64', 'cpu')), patch.object(runner.subprocess, 'Popen', side_effect=tool), patch.object(runner.sys, 'argv', ['native-test', '--evidence', str(out)]), patch.dict(runner.os.environ), patch.object(Path, 'read_text', autospec=True, side_effect=Path.read_text) as _read, patch('builtins.print'):
                runner.os.environ.pop('LOOMTERM_NATIVE_MANIFEST', None)
                # /etc/os-release is inventory-only; keep this unit test portable.
                original_read = _read.side_effect
                _read.side_effect = lambda path, *a, **kw: 'synthetic OS' if path.as_posix() == '/etc/os-release' else original_read(path, *a, **kw)
                self.assertEqual(runner.main(), 1)
            summary = json.loads((out / 'ci-summary.json').read_text())
            self.assertIn('native-controller', summary['completed_phases'], summary.get('error'))
            self.assertEqual(summary['status'], 'Fail')
            self.assertEqual(summary['active_phase'], 'clippy')
            self.assertIn('clippy::too_many_arguments src/main.rs:1:1', summary['diagnostic_tail'])
            self.assertNotIn('PRIVATE_RENDERED', json.dumps(summary))
            self.assertTrue(summary['clippy_diagnostics']['complete'])
            self.assertTrue(any('unix-native.py' in item for item in order[:-1]))
            self.assertEqual(order[-1], 'cargo clippy')

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
            with patch.object(runner, 'target', return_value='x86_64-unknown-linux-gnu'), patch.object(runner.shutil, 'which', return_value='synthetic-tool'), patch.object(runner.platform, 'uname', return_value=namedtuple('Uname', 'system node release version machine processor')('Linux', 'fixture', 'kernel', 'version', 'x86_64', 'cpu')), patch.object(runner.subprocess, 'Popen', side_effect=failing_tool), patch.object(runner.sys, 'argv', ['native-test', '--evidence', str(out)]), patch.dict(runner.os.environ), patch('builtins.print'):
                runner.os.environ.pop('LOOMTERM_NATIVE_MANIFEST', None)
                self.assertEqual(runner.main(), 1)
            summary = json.loads((out / 'ci-summary.json').read_text())
            self.assertEqual(summary['status'], 'Fail')
            self.assertEqual(summary['active_phase'], 'version-rustc')
            self.assertIn('exited 1', summary['error'])
            self.assertIn('controlled prerequisite failure', summary['diagnostic_tail'])
            self.assertEqual(summary['completed_phases'], ['lint-configuration'])

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
