"""Opt-in real Unix TUI/Daemon acceptance. No remote access or Agent execution.

Run through tools/test-unix-native.py on a fresh, dedicated native runner.
Raw evidence stays in the explicitly selected private directory.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import re
import secrets
import shlex
import signal
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
import traceback


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding='utf-8')


def identity(pid):
    """Observe registered PIDs only; Linux birth ticks, Darwin ps birth seconds."""
    if sys.platform == 'linux':
        try:
            entry = Path(f'/proc/{pid}')
            fields = (entry / 'stat').read_text().rsplit(')', 1)[1].split()
            return dict(pid=pid, birth=fields[19], uid=entry.stat().st_uid,
                        exited=fields[0] == 'Z', executable=os.readlink(entry / 'exe'),
                        pgrp=int(fields[2]), sid=int(fields[3]), precision='kernel ticks')
        except FileNotFoundError:
            return dict(pid=pid, exited=True)
    result = subprocess.run(['/bin/ps', '-ww', '-p', str(pid), '-o',
                             'uid=,stat=,ppid=,pgid=,lstart=,comm='],
                            capture_output=True, text=True, timeout=3,
                            env=os.environ | {'LC_ALL': 'C'})
    if not result.stdout.strip():
        return dict(pid=pid, exited=True)
    fields = result.stdout.strip().split(None, 9)
    if len(fields) != 10:
        raise ValueError('unexpected native ps identity format')
    return dict(pid=pid, uid=int(fields[0]), exited='Z' in fields[1],
                pgrp=int(fields[3]), sid=os.getsid(pid), birth=' '.join(fields[4:9]),
                executable=fields[9], precision='ps seconds; combined with boot/token')


def same_process(saved):
    current = identity(saved['pid'])
    return not current['exited'] and all(current.get(k) == saved.get(k)
                                         for k in ('birth', 'uid', 'executable'))


def wait_for(check, timeout=12):
    end, last = time.monotonic() + timeout, None
    while time.monotonic() < end:
        try:
            result = check()
            if result:
                return result
        except (OSError, AssertionError) as error:
            last = str(error)
        time.sleep(.05)
    raise TimeoutError(f'bounded assertion: {last}')


def exact(stream, length):
    result = bytearray()
    while len(result) < length:
        block = stream.recv(length - len(result))
        if not block:
            raise EOFError('incomplete IPC frame')
        result.extend(block)
    return bytes(result)


def rejection(payload):
    offset = 0

    def integer():
        nonlocal offset
        tag = payload[offset]
        offset += 1
        if tag <= 250:
            return tag
        size = {251: 2, 252: 4, 253: 8}[tag]
        value = int.from_bytes(payload[offset:offset + size], 'little')
        offset += size
        return value

    assert integer() == 0  # Existing private wire Welcome, not a new protocol.
    version, encoding, present = integer(), integer(), integer()
    assert present == 1
    length = integer()
    reason = payload[offset:offset + length].decode()
    assert offset + length == len(payload) and 'version' in reason.lower()
    return dict(server_version=version, encoding=encoding, reason=reason)


class Host:
    def __init__(self, case, label):
        self.case, self.sequence, self.messages = case, 0, queue.Queue(128)
        self.errors = (case.out / f'{label}.stderr').open('wb')
        self.p = subprocess.Popen([str(case.host), '--exact', 'outer_unix_host',
                                   '--ignored', '--nocapture', '--quiet'],
                                  cwd=case.source, env=case.env | dict(
                                      LOOMTERM_TEST_SOURCE=str(case.source),
                                      LOOMTERM_TEST_CAPTURE=str(case.out / f'{label}.vt')),
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.errors)
        case.hosts.append(self)
        case.register(self.p.pid, 'outer-host')

        def read():
            total = 0
            for line in iter(self.p.stdout.readline, b''):
                total += len(line)
                if total > 8 * 1024 * 1024:
                    self.messages.put({'error': 'host output budget'})
                    return
                if line.startswith(b'{'):
                    self.messages.put(json.loads(line), timeout=5)
            self.messages.put({'error': 'host EOF'}, timeout=5)
        threading.Thread(target=read, daemon=True).start()
        assert self.receive()['event'] == 'armed'

    def receive(self):
        value = self.messages.get(timeout=15)
        assert 'error' not in value and value.get('event') != 'error', value
        return value

    def start(self):
        self.p.stdin.write(b'{"start":true}\n')
        self.p.stdin.flush()

    def ready(self):
        value = self.receive()
        assert value['event'] == 'ready', value
        self.case.register(value['pid'], 'tui')
        return self

    def send(self, **command):
        self.sequence += 1
        self.p.stdin.write((json.dumps(command | dict(id=self.sequence)) + '\n').encode())
        self.p.stdin.flush()

    def command(self, **command):
        self.send(**command)
        value = self.receive()
        assert value.get('id') == self.sequence, value
        return value

    def text(self, text):
        return self.command(input=text)

    def visible(self, text):
        return wait_for(lambda: text in self.command()['screen'])

    def select_test_split(self, pane):
        # New clients have independent focus. This fixture is the right split of
        # tab 2; use existing TUI actions, then prove input reaches that child.
        self.case.result['phase'] = 'reattach-tab-focus'
        self.text('\x022')
        wait_for(lambda: self.case.focused()['tab_id'] == pane['tab_id'])
        # Directional navigation uses the client's current snapshot. Do not issue
        # it until this client's rendered tab contains the fixture continuation.
        self.visible('"event":"continuation"')
        self.case.result['phase'] = 'reattach-pane-focus'
        self.text('\x02l')
        wait_for(lambda: self.case.focused()['pane_id'] == pane['pane_id'])
        self.case.result['phase'] = 'reattach-marker'
        challenge = self.case.token + 'r'
        self.text('MARK ' + challenge + '\r')
        return wait_for(lambda: self.case.fixture_event(pane, 'marker_ack', value=challenge))

    def detach(self):
        self.send(input='\x02q')
        value = self.receive()
        if value.get('id') == self.sequence:
            value = self.receive()
        assert value.get('event') == 'exit' and value['code'] == 0, value
        assert self.p.wait(5) == 0
        return time.time_ns()


class Case:
    def __init__(self, args, label):
        self.host, self.exe, self.fixture = args.host.resolve(), args.exe.resolve(), args.fixture.resolve()
        self.token, self.hosts, self.processes, self.events = secrets.token_hex(8), [], [], []
        self.out = args.evidence.resolve() / (label + '-' + self.token)
        self.out.mkdir(parents=True)
        # Short socket root is independent of potentially long checkout/artifact paths.
        base = Path(tempfile.mkdtemp(prefix='lt-m2-', dir='/tmp')).resolve()
        self.root = base / 'product'
        self.source = base / "中文 项目 O'Neil"
        self.source.mkdir()
        self.child_dir = self.source / '子目录 child'
        self.child_dir.mkdir()
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(('LOOMTERM_', 'HERDR_'))}
        self.env.update(LOOMTERM_ROOT=str(self.root), LOOMTERM_SESSION='m2-' + self.token[:8])
        self.api_path = self.root / 'runtime' / self.env['LOOMTERM_SESSION'] / 'loomterm.sock'
        self.client_path = self.api_path.with_name('loomterm-client.sock')
        self.result = dict(status='Fail', root=str(self.root), label=label, phase='prepare', tests={})
        save(self.out / 'result.json', self.result)

    def record(self, event, **fields):
        self.events.append(dict(time_ns=time.time_ns(), event=event, **fields))
        save(self.out / 'timeline.json', self.events)

    def register(self, pid, role):
        observed = identity(pid)
        assert not observed['exited'] and observed['uid'] == os.getuid(), observed
        self.processes.append(observed | dict(role=role))
        save(self.out / 'processes.json', self.processes)
        return observed

    def prepare(self):
        assert len(os.fsencode(self.client_path)) <= (107 if sys.platform == 'linux' else 103)
        result = subprocess.run([str(self.exe), '--check'], env=self.env, cwd=self.source,
                                capture_output=True, timeout=10)
        assert result.returncode == 0, result.stderr
        self.record('product_check', output=json.loads(result.stdout))
        shell = self.root / 'controlled-shell'
        shell.write_text('#!/bin/sh\nunset BASH_ENV ENV PROMPT_COMMAND\nexec /bin/bash --noprofile --norc -i\n')
        shell.chmod(0o700)
        (self.root / 'config/config.toml').write_text(
            'onboarding = false\n[loomterm]\nagent_integrations = false\nshell_no_profile = true\n'
            '[terminal]\ndefault_shell = ' + json.dumps(str(shell)) + '\nshell_mode = "non_login"\n'
            'new_cwd = ' + json.dumps(str(self.source), ensure_ascii=False) + '\n'
            '[session]\nresume_agents_on_restore = false\n[ui]\nprompt_new_tab_name = false\n'
            '[update]\nversion_check = false\nmanifest_check = false\n', encoding='utf-8')

    def api(self, method, params=None):
        assert method in {'ping', 'session.snapshot', 'pane.read', 'pane.process_info', 'server.stop',
                          'workspace.source.set', 'workspace.rename'}
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
            stream.settimeout(5)
            stream.connect(str(self.api_path))
            stream.sendall((json.dumps(dict(id='m2', method=method, params=params or {})) + '\n').encode())
            data = bytearray()
            while b'\n' not in data:
                block = stream.recv(8192)
                if not block:
                    raise EOFError('API closed')
                data.extend(block)
                assert len(data) <= 1_048_576, 'API response budget'
            value = json.loads(data.split(b'\n')[0])
            assert 'error' not in value, value
            return value['result']

    def snapshot(self):
        return self.api('session.snapshot')['snapshot']

    def shape(self, panes, tabs):
        def check():
            snapshot = self.snapshot()
            return snapshot if len(snapshot['panes']) == panes and len(snapshot['tabs']) == tabs else None
        return wait_for(check)

    def focused(self):
        snapshot = self.snapshot()
        return next(p for p in snapshot['panes'] if p['pane_id'] == snapshot['focused_pane_id'])

    def fixture_event(self, pane, event, token=None, **fields):
        text = self.api('pane.read', dict(pane_id=pane['pane_id'], source='recent', lines=100))['read']['text']
        for match in re.finditer(r'\{[^{}]*\}', text.replace('\r', '').replace('\n', '')):
            value = json.loads(match.group())
            if value.get('token') == (token or self.token) and value.get('event') == event and all(value.get(k) == v for k, v in fields.items()):
                self.record('fixture', value=value)
                return value

    def finish(self):
        errors = []
        try:
            if not hasattr(self, 'backend') and self.hosts:
                instance = self.api('ping')['capabilities']['loomterm']
                assert instance['namespace'] == self.env['LOOMTERM_SESSION'] and instance['product'] == 'loomterm'
                observed = identity(instance['pid'])
                assert Path(observed['executable']).resolve() == self.exe
                self.backend = self.register(instance['pid'], 'daemon-cleanup-discovery')
            if hasattr(self, 'backend') and same_process(self.backend):
                self.api('server.stop')
                wait_for(lambda: not same_process(self.backend), 15)
        except Exception as error:
            errors.append(repr(error))
        for host in self.hosts:
            try:
                if host.p.poll() is None:
                    host.p.stdin.close()  # host's bounded ordinary SIGTERM path for its direct CLI child
                    host.p.wait(10)
            except Exception as error:
                errors.append(repr(error))
            finally:
                host.errors.close()
        live, after = [], []
        for item in self.processes:
            try:
                current = identity(item['pid'])
                after.append(current)
                if not current['exited'] and all(current.get(k) == item.get(k) for k in ('birth', 'uid', 'executable')):
                    live.append(item)
            except OSError as error:
                errors.append(repr(error))
                after.append(dict(pid=item['pid'], exited=None))
        if live or errors:
            self.result.update(status='Fail', cleanup_errors=errors, live_registered=live)
        self.result['after'] = after
        self.result['retained_owned_root'] = str(self.root)  # preserve unique evidence; no recursive cleanup
        save(self.out / 'result.json', self.result)


def m5_multi_client_viewport(case, first, second):
    """Exercise one daemon pane through two real outer PTYs and local views."""
    token = case.token
    case.result['phase'] = 'm5-private-viewport'
    first.text(f"for ((i=0;i<120;i++)); do printf 'M5H_{token}_%03d\\n' \"$i\"; done\r")
    first.visible(f'M5H_{token}_119')
    second.visible(f'M5H_{token}_119')
    first.text('\x1b[5~')

    def history_rows():
        rows = [int(value) for value in re.findall(rf'M5H_{token}_(\d{{3}})',
                                                    first.command()['screen'])]
        return rows if rows and min(rows) < 80 and 119 not in rows else None

    rows = wait_for(history_rows)
    anchor = rows[len(rows) // 2]
    assert f'M5H_{token}_119' in second.command()['screen']
    second.text(f"printf 'M5NEW_{token}\\n'\r")
    second.visible(f'M5NEW_{token}')
    wait_for(lambda: f'M5H_{token}_{anchor:03d}' in first.command()['screen'])
    assert f'M5NEW_{token}' not in first.command()['screen']
    first.command(resize=[100, 20])
    wait_for(lambda: f'M5H_{token}_' in first.command()['screen'])
    assert f'M5NEW_{token}' not in first.command()['screen']
    assert f'M5NEW_{token}' in second.command()['screen']

    case.result['phase'] = 'm5-finite-eviction'
    # The byte limit is page-granular; 500 rows can fit without any eviction.
    second.text(f"for ((i=0;i<3000;i++)); do printf 'M5E_{token}_%03d_%064d\\n' \"$i\" 0; done\r")
    case.result['phase'] = 'm5-finite-eviction-tail'
    second.visible(f'M5E_{token}_2999')
    case.result['phase'] = 'm5-finite-eviction-reader'
    wait_for(lambda: f'M5E_{token}_' in first.command()['screen'])
    assert f'M5H_{token}_{anchor:03d}' not in first.command()['screen']
    assert f'M5E_{token}_2999' in second.command()['screen']
    case.result['phase'] = 'm5-finite-eviction-return'
    # Repeated page-down advances through however many complete pages survived.
    for _ in range(120):
        if f'M5E_{token}_2999' in first.command()['screen']:
            break
        first.text('\x1b[6~')
    first.visible(f'M5E_{token}_2999')
    case.record('m5_viewport', anchor=anchor, finite_scrollback_bytes=16384,
                first_rows=20, second_tail=f'M5E_{token}_2999')


def acceptance(case, concurrent):
    case.prepare()
    if concurrent:
        with (case.root / 'config/config.toml').open('a', encoding='utf-8') as config:
            config.write('[advanced]\nscrollback_limit_bytes = 16384\n')
    case.result['phase'] = 'cold-start'
    hosts = [Host(case, 'first')]
    if concurrent:
        hosts.append(Host(case, 'second'))
    for host in hosts:
        host.start()  # both outer PTYs are ready before releasing the empty-namespace launches
    for host in hosts:
        host.ready()
    first = hosts[0]
    snapshot = case.shape(1, 1)
    assert not snapshot['agents']
    instance = case.api('ping')['capabilities']['loomterm']
    assert instance['product'] == 'loomterm' and instance['namespace'] == case.env['LOOMTERM_SESSION']
    case.backend = case.register(instance['pid'], 'daemon')
    assert Path(case.backend['executable']).resolve() == case.exe
    pane = case.focused()
    shell_pid = case.api('pane.process_info', dict(pane_id=pane['pane_id']))['process_info']['shell_pid']
    shell = case.register(shell_pid, 'shell')
    case.result['phase'] = 'ipc-identity'
    for path in (case.api_path, case.client_path):
        metadata = path.lstat()
        assert stat.S_ISSOCK(metadata.st_mode) and metadata.st_uid == os.getuid()
        assert stat.S_IMODE(metadata.st_mode) == 0o600
    assert stat.S_IMODE(case.api_path.parent.stat().st_mode) == 0o700
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(5)
        stream.connect(str(case.client_path))
        body = b'\x00\xfc\xff\xff\xff\xff\x50\x18\x00\x00\x00'
        stream.sendall(struct.pack('<I', len(body)) + body)
        length = struct.unpack('<I', exact(stream, 4))[0]
        assert length < 4096
        case.record('wrong_version_rejected', **rejection(exact(stream, length)))
    case.record('instance', instance=instance, pane=pane, shell=shell)
    if concurrent:
        m5_multi_client_viewport(case, first, hosts[1])
    for host in hosts[1:]:
        host.detach()
    case.result['phase'] = 'source-tab-split'
    # Ordinary shell, explicit Source Folder and real TUI layout keys.
    first.text('cd ' + shlex.quote(str(case.child_dir)) + '\r')
    first.text("printf 'CD_DONE_%s\\n' " + case.token + '\r')
    first.visible('CD_DONE_' + case.token)
    first.text('\x02c')
    case.shape(2, 2)
    assert Path(case.focused()['cwd']).samefile(case.source)
    case.register(case.api('pane.process_info', dict(pane_id=case.focused()['pane_id']))['process_info']['shell_pid'], 'tab-shell')
    first.text('\x02v')
    case.shape(3, 2)
    target = case.focused()
    assert Path(target['cwd']).samefile(case.source)
    target_shell = case.register(case.api('pane.process_info', dict(pane_id=target['pane_id']))['process_info']['shell_pid'], 'split-shell')
    first.text(shlex.quote(str(case.fixture)) + ' ' + case.token + '\r')
    ready = wait_for(lambda: case.fixture_event(target, 'ready'))
    fixture = case.register(ready['pid'], 'fixture')
    assert ready['tty'] and ready['pgrp'] == fixture['pgrp'] and ready['sid'] == fixture['sid']
    case.record('runtime_target', instance=instance, pane=target, shell=target_shell, fixture=fixture)
    case.result['phase'] = 'query-resize'
    first.text('MARK ' + case.token + '\r')
    wait_for(lambda: case.fixture_event(target, 'marker_ack', value=case.token))
    first.text('QUERY1\r')
    wait_for(lambda: case.fixture_event(target, 'continuation', phase=1))
    first.text('SIZE\r')
    before = wait_for(lambda: case.fixture_event(target, 'size'))
    first.command(resize=[156, 44])
    time.sleep(.4)
    first.text('SIZE\r')
    # Snapshot shapes may use different geometry fields across adopted revisions;
    # strict native size increase comes from the child records, never UI inference.
    def resized():
        text = case.api('pane.read', dict(pane_id=target['pane_id'], source='recent', lines=100))['read']['text']
        events = [json.loads(m.group()) for m in re.finditer(r'\{[^{}]*\}', text.replace('\r', '').replace('\n', ''))]
        return next((e for e in events if e.get('event') == 'size' and e.get('token') == case.token and e['cols'] > before['cols'] and e['rows'] > before['rows']), None)
    case.record('resize', before=before, after=wait_for(resized))
    case.result['phase'] = 'detach-no-client-query'
    first.text('DETACHQUERY2\r')
    wait_for(lambda: case.fixture_event(target, 'detach_armed'))
    detached = first.detach()
    reply = wait_for(lambda: case.fixture_event(target, 'query_reply', phase=2))
    assert reply['query_time_ns'] > detached
    wait_for(lambda: case.fixture_event(target, 'continuation', phase=2))
    assert same_process(target_shell) and same_process(fixture)
    case.result['phase'] = 'reattach-identity'
    attached = Host(case, 'reattach')
    attached.start()
    attached.ready()
    assert case.api('ping')['capabilities']['loomterm'] == instance
    assert any(p['terminal_id'] == target['terminal_id'] for p in case.shape(3, 2)['panes'])
    assert same_process(target_shell) and same_process(fixture)
    case.result['phase'] = 'reattach-focus'
    attached.select_test_split(target)
    case.record('reattached', instance=instance, pane=target, shell=target_shell, fixture=fixture)
    case.result['phase'] = 'fixture-exit-tail'
    attached.text('EXIT0\r')
    wait_for(lambda: case.fixture_event(target, 'tail', code=0))
    wait_for(lambda: not same_process(fixture))
    case.result['phase'] = 'close-pane'
    attached.text('\x02x')
    case.shape(2, 2)
    wait_for(lambda: not same_process(target_shell))
    attached.detach()
    case.result.update(status='Pass', phase='complete', tests=dict(dual_ipc='Pass', wrong_version='Pass',
                       real_tui='Pass', source_cd_tab_split='Pass', query_reply='Pass', resize='Pass',
                       detach_no_client_reply='Pass', reattach_identity='Pass', close_pane='Pass',
                       concurrent_cold='Pass' if concurrent else 'Pending',
                       m5_private_viewport='Pass' if concurrent else 'Pending',
                       m5_finite_eviction='Pass' if concurrent else 'Pending',
                       signal_descendant_eof='Pending', launchd_gui_shutdown='Pending'))


def exit_source_boundaries(case, _concurrent):
    """Finite native cases; only fixture self-signals and ordinary Shell exit."""
    case.prepare()
    case.result['phase'] = 'boundary-start'
    host = Host(case, 'boundary')
    host.start()
    host.ready()
    initial = case.shape(1, 1)
    workspace, original = initial['workspaces'][0], initial['panes'][0]
    instance = case.api('ping')['capabilities']['loomterm']
    assert instance['product'] == 'loomterm' and instance['namespace'] == case.env['LOOMTERM_SESSION']
    case.backend = case.register(instance['pid'], 'daemon')
    assert Path(case.backend['executable']).resolve() == case.exe
    original_shell = case.register(case.api('pane.process_info', dict(pane_id=original['pane_id']))['process_info']['shell_pid'], 'original-shell')
    host.visible('1')  # Then require an actual Shell response, not spawn readiness.
    marker = 'READY_' + case.token
    host.text("printf 'READY_%s\\n' " + case.token + '\r')
    host.visible(marker)
    case.result['phase'] = 'source-name-change'
    label = '测试 项目'
    renamed = case.api('workspace.rename', dict(workspace_id=workspace['workspace_id'], label=label))['workspace']
    assert renamed['workspace_id'] == workspace['workspace_id'] and renamed['label'] == label
    source = case.source.parent / "第二 中文 Source O'Neil"
    source.mkdir()
    updated = case.api('workspace.source.set', dict(workspace_id=workspace['workspace_id'], source_folder=str(source)))['workspace']
    assert updated['workspace_id'] == workspace['workspace_id'] and updated['label'] == label
    assert Path(updated['source_folder']).samefile(source)
    retained = case.shape(1, 1)['panes'][0]
    assert retained['terminal_id'] == original['terminal_id'] and Path(retained['cwd']).samefile(case.source)
    assert same_process(original_shell)
    host.text('\x02c')
    case.shape(2, 2)
    pane = case.focused()
    assert Path(pane['cwd']).samefile(source)
    shell = case.register(case.api('pane.process_info', dict(pane_id=pane['pane_id']))['process_info']['shell_pid'], 'boundary-shell')
    host.text("printf 'CWD_%s:%s\\n' " + case.token + ' "$PWD"\r')
    host.visible('CWD_' + case.token + ':' + str(source))
    case.record('fixture', value=dict(event='source_change', token=case.token, stable_workspace=True,
                                    retained_terminal=True, new_shell_cwd=True, unicode_name=True))
    for suffix, command, status in [('a', 'EXIT7', 7), ('b', 'SIGNAL', 128 + signal.SIGTERM), ('c', 'HOLD', 0)]:
        case.result['phase'] = 'boundary-' + command.lower()
        token = case.token + suffix
        host.text(shlex.quote(str(case.fixture)) + ' ' + token + '\r')
        ready = wait_for(lambda: case.fixture_event(pane, 'ready', token=token))
        child = case.register(ready['pid'], 'boundary-' + suffix)
        assert ready['pgrp'] == child['pgrp'] and ready['sid'] == child['sid'] == shell['sid']
        host.text(command + '\r')
        event = 'tail' if command == 'EXIT7' else 'signal_tail' if command == 'SIGNAL' else 'descendant'
        tail = wait_for(lambda: case.fixture_event(pane, event, token=token))
        descendant = None
        if command == 'HOLD':
            descendant = case.register(tail['pid'], 'held-pty-descendant')
            assert descendant['pgrp'] == child['pgrp'] and descendant['sid'] == child['sid']
        wait_for(lambda: not same_process(child))
        if descendant:
            assert same_process(descendant), 'descendant must outlive its fixture parent'
            wait_for(lambda: case.fixture_event(pane, 'descendant_tail', token=token))
            wait_for(lambda: not same_process(descendant))
        # The command echo contains %s, never the expected numeric response.
        prefix = 'STATUS_' + token + '_'
        host.text("printf '" + prefix + "%s\\n' \"$?\"\r")
        host.visible(prefix + str(status))
        assert same_process(shell)
        case.record('fixture', value=dict(event='exit_boundary', token=case.token, mode=command,
                                        status=status, parent_exited=True, shell_retained=True,
                                        pgrp=child['pgrp'], sid=child['sid'],
                                        descendant_exited=not descendant or not same_process(descendant)))
    case.result['phase'] = 'natural-shell-eof'
    host.text('exit 0\r')
    remaining = case.shape(1, 1)
    assert all(p['terminal_id'] != pane['terminal_id'] for p in remaining['panes'])
    wait_for(lambda: not same_process(shell))
    assert same_process(original_shell)
    host.detach()
    case.result.update(status='Pass', phase='complete', tests=dict(source_name_change='Pass',
                       abnormal_exit='Pass', self_signal='Pass', descendant_held_pty='Pass',
                       natural_shell_eof='Pass', abrupt_shell_with_live_descendant='Pending'))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('exe', 'host', 'fixture', 'evidence'):
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    if sys.platform not in ('linux', 'darwin'):
        parser.error('requires a real supported Unix native environment')
    def deadline(_signum, _frame):
        raise TimeoutError('outer watchdog requested ordinary owned cleanup')
    signal.signal(signal.SIGTERM, deadline)
    results = []
    for label, concurrent, execute in [('lifecycle', False, acceptance), ('cold-race', True, acceptance),
                                       ('exit-source-boundaries', False, exit_source_boundaries)]:
        case = Case(args, label)
        try:
            execute(case, concurrent)
        except Exception:
            case.result['traceback'] = traceback.format_exc()
        finally:
            case.finish()
        results.append(dict(label=label, status=case.result['status']))
        if case.result['status'] != 'Pass':
            break
    save(args.evidence / 'summary.json', dict(cases=results))
    print(json.dumps(dict(cases=results)))
    return 0 if len(results) == 3 and all(r['status'] == 'Pass' for r in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
