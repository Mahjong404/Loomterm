"""Bounded full-Window checker PNG verification; no claim about other images."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import traceback

from PIL import Image
import window_observer as n
from window_win32 import Window


def run(args):
    n.deadline = time.monotonic() + 180
    case = n.Case(args, "window-image-native")
    case.root = Path(__file__).resolve().parents[2] / "target" / ("w3-" + case.nonce)
    case.env["LOOMTERM_ROOT"] = str(case.root)
    case.result["root"] = str(case.root)
    process = window = None
    try:
        case.prepare()
        env = case.env | {"LOOMTERM_WINDOW_ROOT": str(case.root / "window")}
        process = subprocess.Popen([
            str(args.launcher.resolve()), "--host", str(args.host.resolve()),
            "--cli", str(args.exe.resolve()), "--root", str(case.root),
            "--cwd", str(case.source), "--session", case.env["LOOMTERM_SESSION"],
        ], creationflags=0x08000000)
        case.register(process.pid, args.launcher.resolve(), "window-launcher")
        socket = n.wait_for(lambda: next((case.root / "window/runtime").glob("gui-sock-*"), None))
        env["WEZTERM_UNIX_SOCKET"] = str(socket)
        pid = int(socket.name.removeprefix("gui-sock-"))
        case.register(pid, args.host.resolve(), "window-host")
        window = n.wait_for(lambda: Window.find(pid))
        window.resize(1500, 1000)

        def outer(*command):
            result = subprocess.run([
                str(args.control.resolve()), "--skip-config", "cli",
                "--no-auto-start", *command,
            ], env=env, capture_output=True, timeout=12)
            assert result.returncode == 0, result.stderr.decode(errors="replace")
            assert len(result.stdout) < 1_048_576
            return result.stdout.decode("utf-8")

        def send(text):
            outer("send-text", "--pane-id", "0", "--no-paste", text)

        def grid():
            return json.loads(outer("list", "--format", "json"))[0]["size"]

        n.wait_for(lambda: "PS " in outer("get-text", "--pane-id", "0"))
        pane = case.snapshot()["panes"][0]
        shell = case.shell_identity(pane)
        # Read the exact new backend's module paths; no environment or output.
        query = f"(Get-Process -Id {case.backend['pid']}).Modules | Where-Object ModuleName -eq conpty.dll | Select-Object -ExpandProperty FileName"
        modules = subprocess.run(
            [str(args.shell.resolve()), "-NoProfile", "-NonInteractive", "-Command", query],
            capture_output=True, timeout=10,
        )
        assert modules.returncode == 0, modules.stderr.decode(errors="replace")
        loaded = modules.stdout.decode("utf-8-sig").strip()
        expected = args.exe.resolve().parent / "conpty/conpty.dll"
        assert loaded and os.path.normcase(loaded) == os.path.normcase(str(expected)), loaded
        case.result["backend_conpty"] = dict(path=loaded, sha256=hashlib.sha256(expected.read_bytes()).hexdigest())

        command = "& " + n.quote_ps(Path(sys.executable)) + " " + n.quote_ps(args.fixture.resolve())
        send(command + "\r")
        n.wait_for(lambda: "M3_PNG_START" in outer("get-text", "--pane-id", "0"))

        def capture(label):
            path = case.path / (label + ".png")
            window.capture(path)
            with Image.open(path) as bitmap:
                bitmap = bitmap.convert("RGB")
                old_colors = ((240, 16, 240), (16, 240, 16))
                new_colors = ((240, 48, 16), (16, 80, 240))
                old_points, new_points = [], []
                for i, color in enumerate(bitmap.getdata()):
                    if color in old_colors:
                        old_points.append((i % bitmap.width, i // bitmap.width))
                    if color in new_colors:
                        new_points.append((i % bitmap.width, i // bitmap.width))
                points = old_points + new_points
            bounds = [min(x for x, _ in points), min(y for _, y in points),
                      max(x for x, _ in points), max(y for _, y in points)] if points else None
            return dict(file=path.name, pixels=len(points), old_pixels=len(old_points),
                        replacement_pixels=len(new_points), bounds=bounds,
                        geometry=window.geometry(), grid=grid())

        def image_present(label):
            sample = capture(label)
            return sample if sample["pixels"] > 500 else None

        original = n.wait_for(lambda: image_present("original"))
        previous = grid()
        window.resize(1700, 1100)
        n.wait_for(lambda: grid() != previous)
        n.wait_for(lambda: case.snapshot()["layouts"][0]["panes"][0]["rect"]["height"] + 1 == grid()["rows"])
        resized = n.wait_for(lambda: image_present("resized"))
        bounds, size = resized["bounds"], resized["grid"]
        assert bounds[2] - bounds[0] + 1 == 8 * size["pixel_width"] // size["cols"]
        assert bounds[3] - bounds[1] + 1 == 4 * size["pixel_height"] // size["rows"]
        send("1..80 | ForEach-Object { Write-Output 'M3_IMAGE_SCROLL' }\r")
        n.wait_for(lambda: "M3_IMAGE_SCROLL" in outer("get-text", "--pane-id", "0"))
        n.wait_for(lambda: capture("scrolled")["pixels"] == 0)
        send(command + "\r")
        n.wait_for(lambda: image_present("redrawn"))
        send(command + " --replace\r")
        n.wait_for(lambda: "M3_PNG_REPLACE" in outer("get-text", "--pane-id", "0"))
        replaced = n.wait_for(lambda: (sample if (sample := capture("replaced"))["replacement_pixels"] > 500 else None))
        assert replaced["old_pixels"] == 0, replaced
        send(command + " --replace\r")
        repeated = n.wait_for(lambda: (sample if (sample := capture("repeated"))["replacement_pixels"] > 500 else None))
        assert repeated["old_pixels"] == 0 and repeated["replacement_pixels"] == replaced["replacement_pixels"], repeated
        send(command + " --invalid\r")
        n.wait_for(lambda: "M3_PNG_BAD" in outer("get-text", "--pane-id", "0"))
        invalid = n.wait_for(lambda: (sample if (sample := capture("invalid"))["pixels"] == 0 else None))
        send(command + "\r")
        n.wait_for(lambda: image_present("recovered"))
        send(command + " --delete\r")
        n.wait_for(lambda: capture("deleted")["pixels"] == 0)
        send(command + " --overbudget\r")
        n.wait_for(lambda: "M3_PNG_OVERBUDGET" in outer("get-text", "--pane-id", "0"), 20)
        overbudget = n.wait_for(lambda: (sample if (sample := capture("overbudget"))["pixels"] == 0 else None), 15)
        case.result.update(
            status="Pass", original=original, resized=resized, replaced=replaced,
            repeated=repeated, invalid=invalid, overbudget=overbudget, shell=shell,
            scope="one PNG ID; native resize, 8x4 cells, output scroll-out, redraw, replacement, repeat, truncated replacement rejection, recovery, delete, overbudget rejection",
            pending=["malformed/oversized visible error feedback", "resource reclamation"],
        )
    except Exception:
        case.result.update(status="Fail", traceback=traceback.format_exc())
    finally:
        if window:
            try:
                window.close()
            except Exception:
                pass
        if case.backend and n.identity(case.backend["pid"]) == case.backend:
            try:
                case.api("server.stop")
                n.wait_for(lambda: n.identity(case.backend["pid"])["exited"], 15)
            except Exception as error:
                case.result.update(status="Fail", cleanup_error=repr(error))
        if process:
            try:
                process.wait(12)
            except subprocess.TimeoutExpired:
                process.kill()  # only the directly created launcher handle
                case.result.update(status="Fail", cleanup_timeout=True)
        case.finish()
    print(json.dumps(dict(path=str(case.path), result=case.result), ensure_ascii=False))
    return 0 if case.result["status"] == "Pass" else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("launcher", "host", "control", "exe", "shell", "fixture", "evidence"):
        parser.add_argument("--" + name, required=True, type=Path)
    sys.exit(run(parser.parse_args()))
