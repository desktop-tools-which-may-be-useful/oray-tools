#!/usr/bin/env python3
"""Generate repo/windows/index.html for the Windows binaries.

Usage: mkindex.py <version> <windows-dir>
"""
import os
import sys


def main() -> None:
    version, win_dir = sys.argv[1], sys.argv[2]
    base = "https://desktop-tools-which-may-be-useful.github.io/oray-tools/windows"
    files = []
    for f in sorted(os.listdir(win_dir)):
        if f.endswith(".exe"):
            p = os.path.join(win_dir, f)
            sha = open(p + ".sha256").read().strip()
            files.append((f, os.path.getsize(p), sha))
    rows = "\n".join(
        f"<tr><td><a href=\"{base}/{f}\">{f}</a></td><td>{size}B</td><td><code>{sha}</code></td></tr>"
        for f, size, sha in files
    )
    print(f"""<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<title>oray-tools for Windows</title></head>
<body><h1>oray-tools for Windows v{version}</h1>
<p>Self-contained executables (statically linked CRT, no extra DLLs). Run from
PowerShell or CMD. On first run Windows SmartScreen may warn about the unsigned
binary &mdash; click <em>More info &gt; Run anyway</em>.</p>
<table border="1" cellpadding="6"><tr><th>Binary</th><th>Size</th><th>SHA256</th></tr>
{rows}
</table></body></html>""")


if __name__ == "__main__":
    main()
