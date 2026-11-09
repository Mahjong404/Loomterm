"""Check public Markdown local file links; add private Markdown paths explicitly.

Does not crawl private research, fetch URLs, or validate anchors/Mermaid.
Run from any directory. No third-party dependencies; no file writes.
"""

from pathlib import Path
import os
import re
import sys
from urllib.parse import unquote


ROOT = Path(__file__).resolve().parents[1]


def main():
    files = list(ROOT.glob("*.md"))
    for directory, children, names in os.walk(ROOT / "docs"):
        children[:] = [name for name in children if name != "local"]
        files += [Path(directory) / name for name in names if name.endswith(".md")]
    files += [ROOT / name for name in sys.argv[1:]]
    errors = []
    for path in sorted(set(files)):
        source = path.read_text(encoding="utf-8-sig")
        source = re.sub(r"```.*?```", "", source, flags=re.S)
        for match in re.finditer(r"!?\[[^\]]*\]\((<[^>]+>|[^\s)]+)(?:\s+\"[^\"]*\")?\)", source):
            target = match[1].strip("<>").split("#", 1)[0]
            if not target or re.match(r"[a-zA-Z][\w+.-]*:", target):
                continue
            if not (path.parent / unquote(target)).exists():
                errors.append(f"{path.relative_to(ROOT)}: missing {target}")
    print(f"Checked {len(set(files))} Markdown files; {len(errors)} missing local targets.")
    for error in errors:
        print(error)
    return bool(errors)


if __name__ == "__main__":
    sys.exit(main())
