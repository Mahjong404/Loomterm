"""Windows observation/cleanup support for the Window-only native fixture.

Derived from Loomterm's M1 fixture; contains no input injection or workspace
creation API. Evidence is private and must not be published as a CI artifact.
"""
from pathlib import Path
import ctypes as c
from ctypes import wintypes as w
import hashlib
import json
import re
import os
import secrets
import subprocess
import time
import traceback

deadline = float('inf')
cancel_file = None
cleaning = False

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



def check_deadline():
    if not cleaning and (time.monotonic() >= deadline or (cancel_file and cancel_file.exists())):
        raise TimeoutError('controller deadline/cancellation')

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

def pipe_name(endpoint):
    canonical = str(endpoint.resolve()).removeprefix('\\\\?\\').replace('\\', '/')
    canonical = ''.join(ch.lower() if ch.isascii() else ch for ch in canonical)
    return '\\\\.\\pipe\\loomterm-v1-' + hashlib.sha256(canonical.encode()).hexdigest()

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
        assert method in {'ping', 'session.snapshot', 'pane.process_info', 'pane.read', 'server.stop'}
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

    def shell_identity(self, pane):
        info = self.api('pane.process_info', dict(pane_id=pane['pane_id']))['process_info']
        return self.register(info['shell_pid'], self.shell, 'shell-' + pane['pane_id'])

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
