"""Real Windows TUI controls plus explicitly scoped Workspace API acceptance."""
from pathlib import Path
import argparse
import concurrent.futures
import ctypes as c
from ctypes import wintypes as w
import hashlib
import json
import os
import queue
import re
import secrets
import subprocess
import threading
import time
import cli_probes
import traceback

deadline = float('inf')
cancel_file = None
cleaning = False


def check_deadline():
    if not cleaning and (time.monotonic() >= deadline or (cancel_file and cancel_file.exists())):
        raise TimeoutError('controller deadline/cancellation')

k = c.WinDLL('kernel32', use_last_error=True)
k.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]
k.OpenProcess.restype = w.HANDLE
k.CloseHandle.argtypes = [w.HANDLE]
k.GetProcessTimes.argtypes = [w.HANDLE] + [c.POINTER(w.FILETIME)] * 4
k.QueryFullProcessImageNameW.argtypes = [w.HANDLE, w.DWORD, w.LPWSTR, c.POINTER(w.DWORD)]
k.CreateFileW.argtypes = [w.LPCWSTR, w.DWORD, w.DWORD, c.c_void_p, w.DWORD, w.DWORD, w.HANDLE]
k.CreateFileW.restype = w.HANDLE
k.PeekNamedPipe.argtypes = [w.HANDLE, c.c_void_p, w.DWORD, c.c_void_p, c.POINTER(w.DWORD), c.c_void_p]
k.GetNamedPipeServerProcessId.argtypes = [w.HANDLE, c.POINTER(w.ULONG)]
k.ReadFile.argtypes = [w.HANDLE, c.c_void_p, w.DWORD, c.POINTER(w.DWORD), c.c_void_p]
k.WriteFile.argtypes = [w.HANDLE, c.c_void_p, w.DWORD, c.POINTER(w.DWORD), c.c_void_p]


def identity(pid):
    handle = k.OpenProcess(0x1000 | 0x100000, False, pid)
    if not handle:
        error = c.get_last_error()
        if error not in (87, 1168):
            raise OSError(error, 'owned process identity unavailable')
        return dict(pid=pid, exited=True, error=error)
    try:
        fields = [w.FILETIME() for _ in range(4)]
        assert k.GetProcessTimes(handle, *[c.byref(x) for x in fields])
        exited = bool(fields[1].dwHighDateTime or fields[1].dwLowDateTime)
        capacity = w.DWORD(32768)
        image = c.create_unicode_buffer(capacity.value)
        image_ok = k.QueryFullProcessImageNameW(handle, 0, image, c.byref(capacity))
        image_error = c.get_last_error() if not image_ok else None
        if not image_ok and not exited:
            # The process may exit between the time and image queries.
            assert k.GetProcessTimes(handle, *[c.byref(x) for x in fields])
            exited = bool(fields[1].dwHighDateTime or fields[1].dwLowDateTime)
        if not image_ok and not exited:
            raise OSError(image_error, 'live owned process image unavailable')
        return dict(pid=pid, creation_filetime=(fields[0].dwHighDateTime << 32) | fields[0].dwLowDateTime,
                    exe=image.value if image_ok else None, exited=exited,
                    **(dict(image_error=image_error) if not image_ok else {}))
    finally:
        k.CloseHandle(handle)


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding='utf-8')


def observe_registered(item):
    current = identity(item['pid'])
    if current.get('creation_filetime') not in (None, item['creation_filetime']):
        return dict(pid=item['pid'], exited=True, pid_reused=True, role=item['role'])
    return current | dict(role=item['role'])


def wait_for(check, timeout=12):
    deadline, last = time.monotonic() + timeout, None
    while time.monotonic() < deadline:
        check_deadline()
        try:
            result = check()
            if result:
                return result
        except (OSError, AssertionError) as error:
            last = repr(error)
        time.sleep(.05)
    raise TimeoutError(f'bounded assertion timed out: {last}')


def quote_ps(value):
    return "'" + str(value).replace("'", "''") + "'"


def same_directory(left, right):
    # Windows canonical paths may carry the extended \\?\ namespace. Compare
    # actual filesystem identity rather than spelling, case or a trailing slash.
    return Path(left).samefile(Path(right))


def pipe_name(endpoint):
    canonical = str(endpoint.resolve()).removeprefix('\\\\?\\').replace('\\', '/')
    canonical = ''.join(ch.lower() if ch.isascii() else ch for ch in canonical)
    return '\\\\.\\pipe\\loomterm-v1-' + hashlib.sha256(canonical.encode()).hexdigest()


class Host:
    def __init__(self, case, name):
        self.case, self.name = case, name
        self.messages, self.events = queue.Queue(maxsize=128), []
        self.sequence = 0
        env = case.env | dict(LOOMTERM_TEST_SOURCE=str(case.source),
                              LOOMTERM_TEST_CAPTURE=str(case.path / f'{name}.vt'))
        self.stderr = (case.path / f'{name}.stderr').open('wb')
        self.p = subprocess.Popen([str(case.host), '--exact', 'outer_conpty_host', '--ignored', '--nocapture', '--quiet'],
                                  cwd=case.source, env=env, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=self.stderr, creationflags=0x08000000)
        case.hosts.append(self)
        case.register(self.p.pid, case.host, f'{name}-host')

        def read():
            total = 0
            with (case.path / f'{name}.host-stdout.jsonl').open('wb') as transcript:
                for raw in self.p.stdout:
                    total += len(raw)
                    if total > 16 * 1024 * 1024:
                        self.messages.put(dict(event='error', message='host stdout budget'))
                        return
                    transcript.write(raw)
                    transcript.flush()
                    text = raw.decode('utf-8', errors='replace').strip()
                    if '{' in text:
                        try:
                            value = json.loads(text[text.index('{'):])
                        except ValueError as error:
                            value = dict(event='error', message=repr(error))
                        value['received_time_ns'] = time.time_ns()
                        self.events.append(value)
                        self.messages.put(value)
            self.messages.put(dict(event='eof'))
        threading.Thread(target=read, daemon=True).start()
        assert self.receive()['event'] == 'armed'

    def receive(self, timeout=20):
        end = time.monotonic() + timeout
        while True:
            check_deadline()
            try:
                value = self.messages.get(timeout=min(.1, max(.001, end - time.monotonic())))
                break
            except queue.Empty:
                if time.monotonic() >= end:
                    raise TimeoutError('outer host response deadline')
        assert value.get('event') not in ['error', 'eof'], value
        return value

    def start(self, barrier=None):
        if barrier:
            barrier.wait(timeout=10)
        self.p.stdin.write(b'{"start":true}\n')
        self.p.stdin.flush()
        event = self.receive()
        assert event['event'] == 'ready', event
        self.client = self.case.register(event['pid'], self.case.exe, f'{self.name}-cli')
        return self

    def send(self, **command):
        self.sequence += 1
        packet = json.dumps(command | dict(id=self.sequence), ensure_ascii=False) + '\n'
        self.p.stdin.write(packet.encode('utf-8'))
        self.p.stdin.flush()
        return self.sequence

    def command(self, **command):
        request_id = self.send(**command)
        value = self.receive()
        assert value.get('id') == request_id, value
        return value

    def text(self, text):
        self.case.event('keyboard', host=self.name, text=text)
        return self.command(input=text)

    def key(self, suffix):
        return self.text('\x02' + suffix)

    def screen(self):
        return self.command()['screen']

    def visible(self, expected):
        def check():
            screen = self.screen()
            return screen if expected in screen else None
        screen = wait_for(check)
        self.case.event('visible_marker', host=self.name, expected=expected, screen=screen)
        return screen

    def detach(self):
        self.case.event('keyboard', host=self.name, text='\x02q')
        request_id = self.send(input='\x02q')
        try:
            event = self.receive()
            if event.get('id') == request_id:
                event = self.receive()
            assert event.get('event') == 'exit' and event['code'] == 0, event
            assert self.p.wait(5) == 0
            self.stderr.close()
            self.detached_ns = time.time_ns()
            self.case.event('detached', host=self.name, request_id=request_id,
                            client_after=identity(self.client['pid']))
        finally:
            save(self.case.path / f'{self.name}.json', self.events)


class Case:
    def __init__(self, args, label):
        self.exe, self.host, self.shell = args.exe.resolve(), args.host.resolve(), args.shell.resolve()
        self.nonce = secrets.token_hex(8)
        self.path = args.evidence.resolve() / (label + '-' + self.nonce)
        self.path.mkdir(parents=True, exist_ok=False)
        self.root, self.source = self.path / 'productRoot', self.path / "中文 项目 O'Neil"
        (self.source / '子目录 child').mkdir(parents=True)
        self.env = os.environ.copy()
        # Only product-specific selectors are reset. HOME/USERPROFILE/XDG and
        # native Agent source directories retain their ordinary values.
        for key in list(self.env):
            if key.upper().startswith(('HERDR_', 'LOOMTERM_')):
                del self.env[key]
        self.env.update(LOOMTERM_ROOT=str(self.root), LOOMTERM_SESSION='native-' + self.nonce)
        self.hosts, self.registry, self.timeline = [], [], []
        self.backend = None
        self.result = dict(status='Fail', label=label, nonce=self.nonce, root=str(self.root), source=str(self.source),
                           binary_sha256=hashlib.sha256(self.exe.read_bytes()).hexdigest(),
                           host_sha256=hashlib.sha256(self.host.read_bytes()).hexdigest(),
                           fixture_sha256=hashlib.sha256(args.fixture.read_bytes()).hexdigest(),
                           shell_sha256=hashlib.sha256(self.shell.read_bytes()).hexdigest())

    def prepare(self):
        self.event('product_check', output=self.cli('--check'))
        config = ('onboarding = false\n[loomterm]\nagent_integrations = false\nshell_no_profile = true\n'
                  '[update]\nversion_check = false\nmanifest_check = false\n'
                  '[terminal]\ndefault_shell = ' + json.dumps(str(self.shell)) + '\n'
                  'shell_mode = "non_login"\nnew_cwd = ' + json.dumps(str(self.source), ensure_ascii=False) + '\n'
                  '[session]\nresume_agents_on_restore = false\n'
                  '[ui]\nprompt_new_tab_name = false\n')
        (self.root / 'config' / 'config.toml').write_text(config, encoding='utf-8')
        endpoint = self.root / 'runtime' / self.env['LOOMTERM_SESSION'] / 'loomterm.sock'
        self.pipe = pipe_name(endpoint)
        self.client_pipe = pipe_name(endpoint.with_name('loomterm-client.sock'))
        self.event('prepared', api_pipe=self.pipe, client_pipe=self.client_pipe)

    def event(self, event, **fields):
        self.timeline.append(dict(time_ns=time.time_ns(), event=event, **fields))
        save(self.path / 'timeline.json', self.timeline)

    def register(self, pid, executable, role):
        item = identity(pid)
        assert Path(item['exe']).resolve() == executable.resolve(), item
        if any(saved['pid'] == pid and saved['creation_filetime'] == item['creation_filetime']
               for saved in self.registry):
            return item
        self.registry.append(item | dict(role=role))
        save(self.path / 'processes.json', self.registry)
        return item

    def cli(self, *args):
        # Initialization/diagnostic entry only. Main UI operations use Host.text.
        process = subprocess.Popen([str(self.exe), *args], cwd=self.source, env=self.env,
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   creationflags=0x08000000)
        self.register(process.pid, self.exe, 'diagnostic-' + '-'.join(args))
        try:
            out, err = process.communicate(timeout=12)
        except subprocess.TimeoutExpired:
            process.kill()  # exact directly-created diagnostic process
            process.communicate(timeout=3)
            raise
        assert process.returncode == 0, (args, out, err)
        assert len(out) + len(err) <= 1_048_576, 'diagnostic output budget'
        return json.loads(out)

    def api(self, method, params=None, expected_error=None):
        check_deadline()
        assert method in {'ping', 'session.snapshot', 'pane.process_info', 'pane.read', 'server.stop',
                          'workspace.rename', 'workspace.source.set'}
        handle = k.CreateFileW(self.pipe, 0xC0000000, 0, None, 3, 0, None)
        if handle == c.c_void_p(-1).value:
            raise OSError(c.get_last_error(), 'open exact product API pipe')
        try:
            pid = w.ULONG()
            assert k.GetNamedPipeServerProcessId(handle, c.byref(pid))
            server = identity(pid.value)
            assert Path(server['exe']).resolve() == self.exe
            if self.backend:
                assert server == self.backend, 'backend identity changed'
            else:
                self.backend = self.register(pid.value, self.exe, 'backend')
            packet = (json.dumps(dict(id='native-evidence', method=method, params=params or {})) + '\n').encode()
            count = w.DWORD()
            assert k.WriteFile(handle, packet, len(packet), c.byref(count), None) and count.value == len(packet)
            data = bytearray()
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                check_deadline()
                available = w.DWORD()
                if not k.PeekNamedPipe(handle, None, 0, None, c.byref(available), None):
                    raise OSError(c.get_last_error(), 'API pipe closed before response')
                if available.value:
                    assert len(data) + available.value <= 1_048_576, 'API output budget'
                    buffer = c.create_string_buffer(available.value)
                    assert k.ReadFile(handle, buffer, len(buffer), c.byref(count), None)
                    data.extend(buffer.raw[:count.value])
                    if b'\n' in data:
                        response = json.loads(data.split(b'\n', 1)[0])
                        if expected_error:
                            assert response.get('error', {}).get('code') == expected_error, response
                            return response['error']
                        assert 'error' not in response, response
                        return response['result']
                time.sleep(.01)
            raise TimeoutError('API observation deadline')
        finally:
            k.CloseHandle(handle)

    def snapshot(self):
        return self.api('session.snapshot')['snapshot']

    def dual_endpoint_identity(self):
        handle = k.CreateFileW(self.client_pipe, 0xC0000000, 0, None, 3, 0, None)
        if handle == c.c_void_p(-1).value:
            raise OSError(c.get_last_error(), 'open exact product client pipe')
        try:
            pid = w.ULONG()
            assert k.GetNamedPipeServerProcessId(handle, c.byref(pid))
            assert identity(pid.value) == self.backend
            self.event('dual_endpoint_owner', backend=self.backend)
            return True
        finally:
            k.CloseHandle(handle)

    def fixture_events(self, pane):
        read = self.api('pane.read', dict(pane_id=pane['pane_id'], source='recent', lines=100))
        compact = read['read']['text'].replace('\r', '').replace('\n', '')
        events = []
        for match in re.finditer(r'\{[^{}]*\}', compact):
            try:
                event = json.loads(match.group())
            except ValueError:
                continue
            if event.get('token') == self.nonce:
                events.append(event)
        return events, read

    def shape(self, panes, tabs):
        def check():
            snapshot = self.snapshot()
            return snapshot if len(snapshot['panes']) == panes and len(snapshot['tabs']) == tabs else None
        snapshot = wait_for(check)
        for pane in snapshot['panes']:
            self.shell_identity(pane)
        self.event('layout', snapshot=snapshot)
        return snapshot

    def focused(self):
        snapshot = self.snapshot()
        return next(p for p in snapshot['panes'] if p['pane_id'] == snapshot['focused_pane_id'])

    def shell_identity(self, pane):
        info = self.api('pane.process_info', dict(pane_id=pane['pane_id']))['process_info']
        return self.register(info['shell_pid'], self.shell, 'shell-' + pane['pane_id'])

    def marker(self, host, phase):
        expected = 'LOOM_' + self.nonce + '_' + phase + '_汉字'
        command = "Write-Output ('LOOM_' + '" + self.nonce + "' + '_" + phase + "_汉字')\r"
        assert expected not in command
        host.text(command)
        host.visible(expected)

    def finish(self):
        global cleaning
        cleaning = True
        errors, after = [], []

        def record(phase, error):
            errors.append(dict(phase=phase, error=repr(error), traceback=traceback.format_exc()))

        try:
            if self.backend is None and self.hosts and hasattr(self, 'pipe'):
                try:
                    wait_for(lambda: self.api('ping'), 5)
                except Exception as error:
                    record('discover-backend', error)
            # Identity includes creation time and image; never stop by PID/name alone.
            if self.backend:
                try:
                    if identity(self.backend['pid']) == self.backend:
                        self.api('server.stop')
                        wait_for(lambda: identity(self.backend['pid'])['exited'], 15)
                except Exception as error:
                    record('stop-backend', error)
            for host in self.hosts:
                try:
                    if host.p.poll() is None:
                        try:
                            host.p.stdin.close()
                        except (BrokenPipeError, OSError) as error:
                            record(host.name + '-close-input', error)
                        try:
                            host.p.wait(8)
                        except subprocess.TimeoutExpired:
                            # Popen retains the exact directly-created host handle.
                            host.p.kill()
                            host.p.wait(3)
                            record(host.name + '-forced-host-stop', TimeoutError('host cleanup deadline'))
                except Exception as error:
                    record(host.name + '-stop', error)
                finally:
                    try:
                        host.stderr.close()
                        save(self.path / f'{host.name}.json', host.events)
                    except Exception as error:
                        record(host.name + '-save', error)
            for item in self.registry:
                try:
                    after.append(observe_registered(item))
                except Exception as error:
                    after.append(dict(pid=item['pid'], role=item['role'], exited=None,
                                      observation_error=repr(error)))
                    record(item['role'] + '-observe', error)
            self.result['registered_after'] = after
            live = [item for item in after if item['exited'] is False]
            if live:
                self.result.update(live_registered_processes=live, status='Fail')
        except Exception as error:
            record('unexpected-cleanup-error', error)
        finally:
            # Preserve primary error/traceback even when observation or cleanup fails.
            if errors:
                self.result.update(cleanup_errors=errors, status='Fail')
            try:
                save(self.path / 'result.json', self.result)
            except Exception as error:
                self.result.update(result_write_error=repr(error), status='Fail')
            cleaning = False


def full_ui(case, fixture):
    cli_probes.entry_isolation(case)
    first = Host(case, 'cold').start()
    snapshot = case.shape(1, 1)
    assert snapshot['agents'] == []
    first_pane = case.focused()
    first_shell = case.shell_identity(first_pane)
    instance = case.api('ping')['capabilities']['loomterm']
    assert instance['product'] == 'loomterm' and instance['namespace'] == case.env['LOOMTERM_SESSION']
    assert instance['pid'] == case.backend['pid']
    wait_for(case.dual_endpoint_identity)
    cli_probes.rejected_wire_version(case)
    case.event('instance', identity=instance, backend=case.backend)
    case.marker(first, 'cold')
    subdir = case.source / '子目录 child'

    def change_directory():
        first.text('Set-Location -LiteralPath ' + quote_ps(subdir) + '\r')
        def observed():
            pane = case.focused()
            return Path(pane.get('foreground_cwd') or pane.get('cwd') or '').resolve() == subdir.resolve()
        wait_for(observed)
    change_directory()
    first.key('c')
    case.shape(2, 2)
    assert Path(case.focused()['cwd']).resolve() == case.source.resolve()
    case.marker(first, 'tabroot')
    change_directory()
    first.key('v')
    case.shape(3, 2)
    split_pane = case.focused()
    assert Path(split_pane['cwd']).resolve() == case.source.resolve()
    split_shell = case.shell_identity(split_pane)
    case.marker(first, 'splitroot')

    def dimensions(phase):
        first.text("Write-Output ('DIM_' + '" + phase + "_' + [Console]::WindowWidth + 'x' + [Console]::WindowHeight)\r")
        regex = re.compile('DIM_' + phase + r'_(\d+)x(\d+)')
        def observed():
            screen = first.screen()
            return screen if regex.search(screen) else None
        screen = wait_for(observed)
        size = tuple(map(int, regex.search(screen).groups()))
        case.event('inner_dimensions', phase=phase, dimensions=size, screen=screen)
        return size
    before = dimensions('before')
    first.command(resize=[156, 44])
    time.sleep(.4)
    after = dimensions('after')
    assert after[0] > before[0] and after[1] > before[1], (before, after)
    hot = Host(case, 'hot').start()
    case.shape(3, 2)
    # Selection is local to each real client. Re-entering text after the other
    # client changes tabs proves the first selection was not silently stolen.
    first.key('1')
    first.visible('LOOM_' + case.nonce + '_cold_汉字')
    case.marker(first, 'focus_a1')
    hot.key('2')
    hot.visible('LOOM_' + case.nonce + '_splitroot_汉字')
    case.marker(hot, 'focus_b1')
    case.marker(first, 'focus_a2')
    case.marker(hot, 'focus_b2')
    markers = {phase: 'LOOM_' + case.nonce + '_' + phase + '_汉字'
               for phase in ['focus_a1', 'focus_a2', 'focus_b1', 'focus_b2']}
    first_read = case.api('pane.read', dict(pane_id=first_pane['pane_id'], source='recent', lines=100))
    hot_read = case.api('pane.read', dict(pane_id=split_pane['pane_id'], source='recent', lines=100))
    first_text, hot_text = first_read['read']['text'], hot_read['read']['text']
    case.event('independent_client_focus_reads', first_client=first.client, hot_client=hot.client,
               first_pane=first_pane, hot_pane=split_pane, markers=markers,
               first_read=first_read, hot_read=hot_read)
    assert first_pane['terminal_id'] != split_pane['terminal_id']
    for phase in ['focus_a1', 'focus_a2']:
        assert markers[phase] in first_text and markers[phase] not in hot_text, (phase, first_read, hot_read)
    for phase in ['focus_b1', 'focus_b2']:
        assert markers[phase] in hot_text and markers[phase] not in first_text, (phase, first_read, hot_read)
    hot.detach()
    assert identity(first_shell['pid']) == first_shell
    assert identity(split_shell['pid']) == split_shell
    first.key('1')
    wait_for(lambda: case.focused()['pane_id'] == first_pane['pane_id'])
    first.text('& ' + quote_ps(fixture.resolve()) + ' ' + case.nonce + '\r')
    first.visible('"event":"ready"')
    ready = wait_for(lambda: next((event for event in case.fixture_events(first_pane)[0] if event.get('event') == 'ready'), None))
    fixture_process = case.register(ready['pid'], fixture.resolve(), 'query-fixture')
    assert fixture_process['creation_filetime'] == ready['creation_filetime']
    first.text('QUERY1\r')
    first.visible('"event":"continuation"')
    first.text('DETACHQUERY2\r')
    first.visible('"event":"detach_armed"')
    first.detach()
    time.sleep(3.3)
    events, read = case.fixture_events(first_pane)
    assert any(e.get('event') == 'continuation' and e.get('phase') == 2 for e in events), read
    query = next(e for e in events if e.get('event') == 'query_reply' and e.get('phase') == 2)
    assert query['query_filetime'] > first.detached_ns // 100 + 116444736000000000, 'query preceded confirmed UI exit'
    case.event('query_without_ui', read=read)
    assert identity(first_shell['pid']) == first_shell
    attached = Host(case, 'reattach').start()
    snapshot = case.shape(3, 2)
    assert next(p for p in snapshot['panes'] if p['pane_id'] == first_pane['pane_id'])['terminal_id'] == first_pane['terminal_id']
    assert case.api('ping')['capabilities']['loomterm'] == instance
    assert identity(first_shell['pid']) == first_shell
    assert identity(fixture_process['pid']) == fixture_process
    attached.text('EXIT0\r')
    attached.visible('"event":"tail"')
    wait_for(lambda: identity(fixture_process['pid'])['exited'])
    case.marker(attached, 'reattach')
    attached.key('2')
    wait_for(lambda: case.focused()['tab_id'] == split_pane['tab_id'])
    attached.key('l')
    wait_for(lambda: case.focused()['pane_id'] == split_pane['pane_id'])
    attached.key('x')
    snapshot = case.shape(2, 2)
    assert all(p['terminal_id'] != split_pane['terminal_id'] for p in snapshot['panes'])
    wait_for(lambda: identity(split_shell['pid'])['exited'])
    case.marker(attached, 'afterclose')
    attached.detach()
    case.result['status'] = 'Pass'


def concurrent_cold(case):
    hosts = [Host(case, 'race-a'), Host(case, 'race-b')]
    barrier = threading.Barrier(2)
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        list(pool.map(lambda host: host.start(barrier), hosts))
    snapshot = case.shape(1, 1)
    pane = snapshot['panes'][0]
    shell = case.shell_identity(pane)
    instance = case.api('ping')['capabilities']['loomterm']
    wait_for(case.dual_endpoint_identity)
    for number, host in enumerate(hosts):
        case.marker(host, 'race' + str(number))
    assert case.api('ping')['capabilities']['loomterm'] == instance
    assert identity(shell['pid']) == shell
    assert case.shape(1, 1)['panes'][0]['terminal_id'] == pane['terminal_id']
    for host in hosts:
        host.detach()
    case.event('concurrent_cold', instance=instance, shell=shell, clients=[host.client for host in hosts])
    case.result['status'] = 'Pass'


def assert_shell_cwd(case, host, expected, phase):
    marker = 'CWD_' + phase + '_True'
    command = ("Write-Output ('CWD_' + '" + phase + "_' + [string]((Get-Location).Path -eq "
               + quote_ps(expected) + '))\r')
    assert marker not in command
    host.text(command)
    host.visible(marker)


def workspace_source(case):
    host = Host(case, 'source-client').start()
    snapshot = case.shape(1, 1)
    workspace = snapshot['workspaces'][0]
    original = snapshot['panes'][0]
    shell = case.shell_identity(original)
    workspace_id = workspace['workspace_id']
    assert Path(workspace['source_folder']).resolve() == case.source.resolve()
    assert_shell_cwd(case, host, case.source, 'initialsource')
    label = '测试 名称'
    renamed = case.api('workspace.rename', dict(workspace_id=workspace_id, label=label))['workspace']
    assert renamed['workspace_id'] == workspace_id and renamed['label'] == label
    host.visible(label)
    missing = case.path / '不存在 source'
    rejected = case.api('workspace.source.set', dict(workspace_id=workspace_id, source_folder=str(missing)),
                        expected_error='invalid_source_folder')
    unchanged = case.snapshot()
    case.event('source_rejection_observation', original_workspace=workspace, original_pane=original,
               rejection=rejected, snapshot=unchanged)
    assert Path(unchanged['workspaces'][0]['source_folder']).resolve() == Path(workspace['source_folder']).resolve()
    assert Path(unchanged['panes'][0]['cwd']).resolve() == Path(original['cwd']).resolve(), (original, unchanged)
    new_source = case.path / '第二 中文 源目录'
    new_source.mkdir()
    changed = case.api('workspace.source.set', dict(workspace_id=workspace_id, source_folder=str(new_source)))['workspace']
    assert changed['workspace_id'] == workspace_id and changed['label'] == label
    case.event('source_updated_observation', expected=str(new_source), response=changed)
    assert same_directory(changed['source_folder'], new_source), changed
    after_source_set = case.snapshot()
    case.event('source_change_observation', original_pane=original, changed_workspace=changed,
               snapshot=after_source_set)
    assert Path(after_source_set['panes'][0]['cwd']).resolve() == Path(original['cwd']).resolve(), (original, after_source_set)
    assert identity(shell['pid']) == shell
    assert_shell_cwd(case, host, case.source, 'oldsource')
    host.key('c')
    snapshot = case.shape(2, 2)
    assert snapshot['workspaces'][0]['workspace_id'] == workspace_id
    new_pane = case.focused()
    assert same_directory(new_pane['cwd'], new_source), new_pane
    new_shell = case.shell_identity(new_pane)
    assert new_shell['pid'] != shell['pid']
    assert_shell_cwd(case, host, new_source, 'newsource')
    retained_pane = next(p for p in snapshot['panes'] if p['pane_id'] == original['pane_id'])
    case.event('retained_pane_observation', original_pane=original, retained_pane=retained_pane,
               snapshot=snapshot)
    assert Path(retained_pane['cwd']).resolve() == Path(original['cwd']).resolve(), (original, retained_pane)
    case.event('workspace_source_acceptance', original_workspace=workspace, renamed=renamed,
               missing_source_rejected=rejected, changed_workspace=changed, snapshot=snapshot)
    host.detach()
    case.result['status'] = 'Pass'


def last_pane_refill(case):
    host = Host(case, 'refill-client').start()
    snapshot = case.shape(1, 1)
    workspace = snapshot['workspaces'][0]
    original = snapshot['panes'][0]
    shell = case.shell_identity(original)
    case.marker(host, 'beforelastclose')
    host.key('x')
    host.visible('Close workspace?')
    host.text('\r')

    def refilled():
        current = case.snapshot()
        return current if (len(current['panes']) == 1 and len(current['tabs']) == 1
                           and current['panes'][0]['terminal_id'] != original['terminal_id']) else None
    snapshot = wait_for(refilled)
    replacement = snapshot['panes'][0]
    replacement_shell = case.shell_identity(replacement)
    assert replacement_shell['pid'] != shell['pid']
    wait_for(lambda: identity(shell['pid'])['exited'])
    assert_shell_cwd(case, host, case.source, 'refillroot')
    # A bounded stability sample detects duplicate/repeated refill while the
    # same client is attached, without introducing a load or performance gate.
    for _ in range(2):
        time.sleep(.2)
        current = case.shape(1, 1)
        assert current['panes'][0]['terminal_id'] == replacement['terminal_id']
        assert current['workspaces'][0]['workspace_id'] == workspace['workspace_id']
        assert current['workspaces'][0]['label'] == workspace['label']
        assert Path(current['workspaces'][0]['source_folder']).resolve() == Path(workspace['source_folder']).resolve()
        assert identity(replacement_shell['pid']) == replacement_shell
    case.event('last_pane_refilled', original=original, replacement=replacement,
               old_shell=shell, replacement_shell=replacement_shell)
    marker = 'ARM_' + case.nonce
    command = "Write-Output ('ARM_' + '" + case.nonce + "'); Start-Sleep -Milliseconds 4000; exit 0\r"
    assert marker not in command
    host.text(command)
    host.visible(marker)
    host.detach()
    wait_for(lambda: identity(replacement_shell['pid'])['exited'], 10)
    empty = case.shape(0, 0)
    assert len(empty['workspaces']) == 1
    assert empty['workspaces'][0]['workspace_id'] == workspace['workspace_id']
    assert Path(empty['workspaces'][0]['source_folder']).resolve() == Path(workspace['source_folder']).resolve()
    time.sleep(.3)
    assert case.shape(0, 0)['workspaces'][0]['workspace_id'] == workspace['workspace_id']
    case.event('natural_exit_without_ui_no_refill', shell=replacement_shell,
               detached_ns=host.detached_ns, snapshot=empty)
    case.result['status'] = 'Pass'


def validate_evidence_directory(path, private_root):
    relative = path.resolve().relative_to(private_root.resolve())
    if not relative.parts or relative.parts[0] not in {'m1', 'm2'}:
        raise ValueError('native evidence must belong to the ignored M1 or M2 stage')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    for name in ['exe', 'host', 'fixture', 'shell', 'evidence']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--deadline-seconds', type=int, default=420)
    parser.add_argument('--cancel-file', type=Path, required=True)
    parser.add_argument('--suite', choices=['core', 'extended', 'all'], default='core')
    args = parser.parse_args()
    deadline = time.monotonic() + args.deadline_seconds
    cancel_file = args.cancel_file
    private_stage = Path(__file__).resolve().parents[2] / 'docs' / 'local' / 'w1-02'
    validate_evidence_directory(args.evidence, private_stage)
    assert os.name == 'nt', 'native Windows only'
    assert all(getattr(args, name).is_file() for name in ['exe', 'host', 'fixture', 'shell'])
    results = []
    tests = []
    if args.suite in ('core', 'all'):
        tests.extend([('full-ui', lambda case: full_ui(case, args.fixture)), ('concurrent-cold', concurrent_cold)])
    if args.suite in ('extended', 'all'):
        tests.extend([('workspace-source', workspace_source), ('last-pane-refill', last_pane_refill)])
    for label, test in tests:
        case = Case(args, label)
        try:
            case.prepare()
            test(case)
        except Exception as error:
            case.result['error'] = repr(error)
            case.result['error_traceback'] = traceback.format_exc()
        finally:
            case.finish()
            results.append(dict(case=str(case.path), **case.result))
            print(json.dumps(results[-1], ensure_ascii=False), flush=True)
    save(args.evidence.resolve() / 'latest.json', results)
    raise SystemExit(0 if all(result['status'] == 'Pass' for result in results) else 1)
