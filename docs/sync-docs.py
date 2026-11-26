#!/usr/bin/env python3
"""Refresh the source listings embedded in docs/DOCUMENTATION.md.

Every block of the form

    <!-- include: path/to/file.rs -->
    ...anything...
    <!-- /include -->

is replaced with a fenced code block holding the current contents of that
file (path relative to the repository root). Run it after changing code:

    python3 docs/sync-docs.py          # rewrite the doc
    python3 docs/sync-docs.py --check  # exit 1 if the doc is out of date
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DOC = ROOT / "docs" / "DOCUMENTATION.md"
LANG = {".rs": "rust", ".toml": "toml", ".nix": "nix", ".h": "c"}
BLOCK = re.compile(r"<!-- include: (\S+) -->\n.*?<!-- /include -->", re.S)


def render(match: re.Match) -> str:
    rel = match.group(1)
    path = ROOT / rel
    code = path.read_text().rstrip("\n")
    lang = LANG.get(path.suffix, "")
    fence = "````" if "```" in code else "```"
    return f"<!-- include: {rel} -->\n{fence}{lang}\n{code}\n{fence}\n<!-- /include -->"


def main() -> int:
    old = DOC.read_text()
    new = BLOCK.sub(render, old)
    if "--check" in sys.argv:
        if new != old:
            print("docs/DOCUMENTATION.md is out of date; run docs/sync-docs.py")
            return 1
        return 0
    DOC.write_text(new)
    return 0


if __name__ == "__main__":
    sys.exit(main())
