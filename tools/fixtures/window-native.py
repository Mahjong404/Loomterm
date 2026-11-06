"""Bounded Windows HWND -> PTY -> full TUI -> Daemon acceptance.

All API calls target a freshly created, isolated test instance. Native resize
and system-menu/titlebar close are distinct from host-control text input.
Screenshots are evidence candidates, never automatic visual Pass assertions.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import traceback

from window_win32 import Window, descendants

import window_observer as n


def run(args):
    n.deadline = time.monotonic() + 180
    case = n.Case(args, "window-gui")
    # Windows AF_UNIX socket names are bounded; evidence stays in --evidence.
    case.root = Path(__file__).resolve().parents[2] / "target" / ("w3-" + case.nonce)
    case.env["LOOMTERM_ROOT"] = str(case.root)
    case.result["root"] = str(case.root)
    processes, windows = [], []
    try:
        case.prepare()
        env = case.env | {"LOOMTERM_WINDOW_ROOT": str(case.root / "window")}
        controller = args.control.resolve()
        socket = None

        def outer(*command):
            result = subprocess.run([str(controller), "--skip-config", "cli", "--no-auto-start", *command],
                                    env=env | {"WEZTERM_UNIX_SOCKET": str(socket)},
                                    capture_output=True, timeout=12)
            assert result.returncode == 0, result.stderr.decode(errors="replace")
            assert len(result.stdout) < 1_048_576
            return result.stdout.decode("utf-8")

        def send(text):
            outer("send-text", "--pane-id", "0", "--no-paste", text)

        def launch():
            nonlocal socket
            old = set((case.root / "window/runtime").glob("gui-sock-*"))
            p = subprocess.Popen([str(args.launcher.resolve()), "--host", str(args.host.resolve()),
                                  "--cli", str(args.exe.resolve()), "--root", str(case.root),
                                  "--cwd", str(case.source), "--session", case.env["LOOMTERM_SESSION"]],
                                 creationflags=0x08000000)
            processes.append(p)
            case.register(p.pid, args.launcher.resolve(), "window-launcher")
            socket = n.wait_for(lambda: next((p for p in (case.root / "window/runtime").glob("gui-sock-*") if p not in old), None))
            pid = int(socket.name.removeprefix("gui-sock-"))
            case.register(pid, args.host.resolve(), "window-host")
            n.wait_for(lambda: case.snapshot())
            window = n.wait_for(lambda: Window.find(pid))
            def tui_identity():
                for child in descendants(pid):
                    observed = n.identity(child)
                    if observed.get("exe") and Path(observed["exe"]).resolve() == args.exe.resolve():
                        return case.register(child, args.exe.resolve(), "window-tui-child")
                return None
            tui = n.wait_for(tui_identity)
            case.event("outer-pty-tui", host_pid=pid, tui=tui)
            windows.append(window)
            n.wait_for(lambda: json.loads(outer("list", "--format", "json")))
            n.wait_for(lambda: "PS " in outer("get-text", "--pane-id", "0"))
            return p, window

        process, window = launch()
        initial = n.wait_for(lambda: case.snapshot())
        pane = initial["panes"][0]
        shell = case.shell_identity(pane)
        backend = case.backend
        case.event("native-window", geometry=window.geometry(), shell=shell, backend=backend)

        dimensions = []
        for i, (width, height) in enumerate([(1060, 730), (1420, 930), (940, 650), (1260, 820)]):
            previous = json.loads(outer("list", "--format", "json"))[0]["size"]
            geometry = window.resize(width, height)
            n.wait_for(lambda: json.loads(outer("list", "--format", "json"))[0]["size"] != previous)
            n.wait_for(lambda: case.snapshot()["layouts"][0]["panes"][0]["rect"]["height"] + (2 if json.loads(outer("list", "--format", "json"))[0]["size"]["cols"] <= 64 else 1) == json.loads(outer("list", "--format", "json"))[0]["size"]["rows"])
            label = "DIM" + str(i) + "_"
            # Native SetWindowPos is the only resize operation in this fixture.
            send("Write-Output ('" + label + "' + [Console]::WindowWidth + 'x' + [Console]::WindowHeight)\r")

            def observed():
                screen = case.api("pane.read", dict(pane_id=pane["pane_id"], source="recent", lines=100))["read"]["text"]
                n.save(case.path / ("resize-" + str(i) + "-last.json"), dict(screen=screen, snapshot=case.snapshot(), host=json.loads(outer("list", "--format", "json"))))
                match = re.search(label + r"(\d+)x(\d+)", screen)
                if not match:
                    return None
                snapshot = case.snapshot()
                rect = snapshot["layouts"][0]["panes"][0]["rect"]
                inner = list(map(int, match.groups()))
                if inner != [rect["width"] - 1, rect["height"]]:
                    return None
                host = json.loads(outer("list", "--format", "json"))[0]
                if host["size"]["rows"] != rect["height"] + (2 if host["size"]["cols"] <= 64 else 1):
                    return None
                return dict(geometry=window.geometry(), host=host, inner_pty=inner, layout=rect, scrollbar_gutter_cols=1)

            observation = n.wait_for(observed)
            dimensions.append(observation)
            case.event("native-resize", **observation)
        assert len({tuple(d["inner_pty"]) for d in dimensions}) == 4
        try:
            window.capture(case.path / "native-window.png")
            case.event("capture", status="candidate", file="native-window.png")
        except Exception as error:
            case.event("capture", status="Pending", error=repr(error))

        if args.input:
            input_result = {"status": "Pending", "IME": "Pending: native Unicode injection is not IME", "Alt_Shift_function_keys": "Pending"}
            try:
                size = window.geometry()["client"]
                window.native_click(size[0] * 3 // 4, size[1] * 2 // 3)
                text = "NATIVE_" + case.nonce
                window.native_text("Write-Output ('NATIVE_' + '" + case.nonce + "_汉字_😀_é')")
                window.native_key(0x0D)
                n.wait_for(lambda: text in case.api("pane.read", dict(pane_id=pane["pane_id"], source="recent", lines=100))["read"]["text"])
                window.native_key(0x11, 0x42)
                window.native_text("c")
                n.wait_for(lambda: len(case.snapshot()["tabs"]) == 2)
                for item in case.snapshot()["panes"]:
                    case.shell_identity(item)
                input_result.update(status="Pass", scope="SendInput Unicode/Return, native click focus and Ctrl+B/c TUI tab shortcut; IME not tested")
                send("\x021")
                n.wait_for(lambda: text in outer("get-text", "--pane-id", "0"))
                window.capture(case.path / "unicode-native.png")
            except Exception as error:
                input_result.update(status="Pending", error=repr(error))
                try:
                    send("\x03\x021")
                except Exception:
                    pass
            case.event("native-input", **input_result)
            case.result["native_input"] = input_result
    
        else:
            case.result["native_input"] = dict(status="Pending", reason="opt-in --input not requested")

        send("& " + n.quote_ps(args.fixture.resolve()) + " " + case.nonce + "\r")
        n.wait_for(lambda: any(x.get("event") == "ready" for x in case.fixture_events(pane)[0]))
        send("DETACHQUERY2\r")
        n.wait_for(lambda: any(x.get("event") == "detach_armed" for x in case.fixture_events(pane)[0]))
        window.close()
        assert process.wait(12) == 0
        closed_ns = time.time_ns()
        case.event("system-close-complete", closed_ns=closed_ns)
        n.wait_for(lambda: any(x.get("event") == "continuation" and x.get("phase") == 2 for x in case.fixture_events(pane)[0]))
        query = next(x for x in case.fixture_events(pane)[0] if x.get("event") == "query_reply" and x.get("phase") == 2)
        assert query["query_filetime"] > closed_ns // 100 + 116444736000000000
        process, window = launch()
        n.wait_for(lambda: "continuation" in outer("get-text", "--pane-id", "0"))
        assert case.backend == backend
        assert n.identity(shell["pid"]) == shell
        assert case.snapshot()["panes"][0]["terminal_id"] == pane["terminal_id"]
        send("EXIT0\r")
        n.wait_for(lambda: "tail" in outer("get-text", "--pane-id", "0"))
        window.close()
        assert process.wait(12) == 0
        case.result.update(status="Pass", scope="native HWND geometry/resize/system close/reopen; visual and input Pending",
                           dimensions=dimensions, backend=backend, shell=shell, terminal_id=pane["terminal_id"], query=query)
    except Exception:
        case.result.update(status="Fail", traceback=traceback.format_exc())
    finally:
        for window in windows:
            try:
                if not n.identity(window.pid)["exited"]:
                    window.close()
            except Exception:
                pass
        if case.backend and n.identity(case.backend["pid"]) == case.backend:
            try:
                case.api("server.stop")
                n.wait_for(lambda: n.identity(case.backend["pid"])["exited"], 15)
            except Exception as error:
                case.result.update(status="Fail", backend_cleanup_error=repr(error))
        for process in processes:
            try:
                process.wait(12)
            except subprocess.TimeoutExpired:
                process.kill()  # exact directly-created launcher handle only
                case.result.update(status="Fail", cleanup_timeout=True)
        case.finish()
    print(json.dumps(dict(path=str(case.path), result=case.result), ensure_ascii=False))
    return 0 if case.result["status"] == "Pass" else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("launcher", "host", "control", "exe", "shell", "fixture", "evidence"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--input", action="store_true", help="Opt in to native input only when the owned Window can receive foreground")
    sys.exit(run(parser.parse_args()))
