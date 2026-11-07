"""Stage the CLI's existing pinned ConPTY runtime; never download or install it."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tempfile
import zipfile


REPO = Path(__file__).resolve().parents[1]
METADATA = REPO / "apps/loomterm-window/conpty.json"


def stage(package, output):
    metadata = json.loads(METADATA.read_text(encoding="utf-8"))
    if package.stat().st_size > 32 * 1024 * 1024:
        raise ValueError("ConPTY package exceeds its input bound")
    if hashlib.sha256(package.read_bytes()).hexdigest() != metadata["package"]["sha256"]:
        raise ValueError("ConPTY package does not match the fixed input")
    expected = {}
    with zipfile.ZipFile(package) as archive:
        for entry in metadata["bundles"]["x86_64"]["files"]:
            data = archive.read(entry["source"])
            if hashlib.sha256(data).hexdigest() != entry["sha256"]:
                raise ValueError("ConPTY native file hash mismatch")
            expected[entry["destination"].removeprefix("conpty/")] = data
    expected["herdr-conpty.json"] = (json.dumps({
        "schema_version": 1, "package": metadata["package"]
    }, indent=2) + "\n").encode()
    notices = {}
    for entry in metadata["notices"]:
        data = (REPO / entry["source"]).read_bytes()
        if hashlib.sha256(data).hexdigest() != entry["sha256"]:
            raise ValueError("ConPTY legal notice hash mismatch")
        notices[Path(entry["source"]).name] = data
    # Validate every input before touching the destination. Refuse replacement
    # of an unknown bundle, including one currently consumed by another build.
    destination = output / "conpty"
    legal = output / "notices/Microsoft.Windows.Console.ConPTY"
    if output.is_symlink() or legal.is_symlink():
        raise ValueError("ConPTY stage path contains a link")
    for name, data in notices.items():
        existing = legal / name
        if existing.is_symlink() or (existing.exists() and existing.read_bytes() != data):
            raise ValueError("existing ConPTY notice differs; choose a fresh staging directory")
    if destination.exists():
        if destination.is_symlink() or any(p.is_symlink() for p in destination.rglob("*")):
            raise ValueError("ConPTY destination contains a link")
        actual = {p.relative_to(destination).as_posix(): p.read_bytes()
                  for p in destination.rglob("*") if p.is_file()}
        if actual != expected:
            raise ValueError("existing ConPTY bundle differs; choose a fresh staging directory")
    else:
        output.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix=".conpty-", dir=output) as temporary:
            prepared = Path(temporary) / "conpty"
            for name, data in expected.items():
                target = prepared / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
            prepared.rename(destination)
    legal.mkdir(parents=True, exist_ok=True)
    for name, data in notices.items():
        (legal / name).write_bytes(data)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    stage(args.package.resolve(), args.output.resolve())
