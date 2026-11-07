"""Finite negative-entry/handshake probes for the full native TUI controller."""
import ctypes as c
from ctypes import wintypes as w
import hashlib
import json
import subprocess
import time


def entry_isolation(case):
    foreign = case.path / 'foreign-product-canary'
    foreign.mkdir()
    canary = foreign / 'config.toml'
    canary.write_text('do not read or adopt this product\n', encoding='utf8')
    before = hashlib.sha256(canary.read_bytes()).hexdigest()

    def run(arguments, environment, success):
        process = subprocess.Popen([str(case.exe), *arguments], cwd=case.source,
                                   env=case.env | environment, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   creationflags=0x08000000)
        case.register(process.pid, case.exe, 'entry-probe')
        try:
            out, err = process.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()  # this directly created, registered diagnostic only
            process.communicate(timeout=3)
            raise
        assert len(out) + len(err) <= 1_048_576
        assert (process.returncode == 0) == success, (arguments, out, err)
        case.event('entry_probe', arguments=arguments, exit=process.returncode,
                   stdout=out.decode('utf8', errors='replace'), stderr=err.decode('utf8', errors='replace'))
        return json.loads(out) if success else None

    run(['--check'], {'LOOMTERM_ROOT': str(foreign)}, False)
    run(['--check'], {'LOOMTERM_SOCKET_PATH': str(foreign / 'herdr.sock')}, False)
    run(['--check'], {'LOOMTERM_CLIENT_SOCKET_PATH': str(foreign / 'herdr-client.sock')}, False)
    run(['--check'], {'LOOMTERM_CONFIG_PATH': str(canary)}, False)
    run(['--check'], {'LOOMTERM_SESSION': '../invalid', 'LOOMTERM_SOCKET_PATH': str(foreign / 'herdr.sock')}, False)
    explicit = 'explicit-' + case.nonce
    value = run(['--session', explicit, '--check'], {'LOOMTERM_SOCKET_PATH': str(foreign / 'herdr.sock')}, True)
    assert value['session'] == explicit and explicit in value['api_endpoint']
    run(['server', 'update-agent-manifests'], {}, False)
    assert hashlib.sha256(canary.read_bytes()).hexdigest() == before
    assert sorted(p.name for p in foreign.iterdir()) == ['config.toml']
    case.event('foreign_canary_unchanged', sha256=before)


def rejected_wire_version(case):
    kernel = c.WinDLL('kernel32', use_last_error=True)
    kernel.CreateFileW.argtypes = [w.LPCWSTR, w.DWORD, w.DWORD, c.c_void_p, w.DWORD, w.DWORD, w.HANDLE]
    kernel.CreateFileW.restype = w.HANDLE
    kernel.CloseHandle.argtypes = [w.HANDLE]
    kernel.PeekNamedPipe.argtypes = [w.HANDLE, c.c_void_p, w.DWORD, c.c_void_p, c.POINTER(w.DWORD), c.c_void_p]
    kernel.ReadFile.argtypes = [w.HANDLE, c.c_void_p, w.DWORD, c.POINTER(w.DWORD), c.c_void_p]
    kernel.WriteFile.argtypes = [w.HANDLE, c.c_void_p, w.DWORD, c.POINTER(w.DWORD), c.c_void_p]
    handle = kernel.CreateFileW(case.client_pipe, 0xC0000000, 0, None, 3, 0, None)
    assert handle != c.c_void_p(-1).value
    try:
        # Original bincode standard: TerminalHello variant 0, u32::MAX version,
        # 80 columns, 24 rows, zero pixel geometry, pixel_mouse=false.
        body = b'\x00\xfc\xff\xff\xff\xff\x50\x18\x00\x00\x00'
        packet = len(body).to_bytes(4, 'little') + body
        count = w.DWORD()
        assert kernel.WriteFile(handle, packet, len(packet), c.byref(count), None)
        assert count.value == len(packet)
        received = bytearray()
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            available = w.DWORD()
            assert kernel.PeekNamedPipe(handle, None, 0, None, c.byref(available), None)
            if available.value:
                assert len(received) + available.value <= 4096
                buffer = c.create_string_buffer(available.value)
                assert kernel.ReadFile(handle, buffer, len(buffer), c.byref(count), None)
                received.extend(buffer.raw[:count.value])
                if len(received) >= 4 and len(received) >= 4 + int.from_bytes(received[:4], 'little'):
                    break
            time.sleep(.01)
        assert len(received) >= 4
        payload = bytes(received[4:])
        assert len(payload) == int.from_bytes(received[:4], 'little')
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

        assert integer() == 0  # Welcome
        version, encoding, present = integer(), integer(), integer()
        assert present == 1
        length = integer()
        reason = payload[offset:offset + length].decode('utf8')
        assert offset + length == len(payload) and 'version' in reason.lower(), reason
        case.event('wrong_wire_version_rejected', sent_version=0xffffffff,
                   server_version=version, encoding=encoding, reason=reason, response_hex=payload.hex())
    finally:
        kernel.CloseHandle(handle)
