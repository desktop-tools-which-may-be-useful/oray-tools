#!/usr/bin/env python3
"""Publish a version's artifacts into the gh-pages release tree.

Usage: mkrelease.py <pages-root> <version> <stage-dir>

Stage files are named "<label>.ext" (e.g. debian-amd64.deb, termux-aarch64.deb,
windows-x86_64.exe). Each file is copied to releases/<version>/oray-tools-<version>-<file>
and published in:
  releases/<version>/manifest.json   per-version asset manifest
  releases.json                      version list (newest first) + all assets
  latest.json                        copy of the latest manifest
"""
import argparse
import hashlib
import json
import os
import shutil

PROJECT = "oray-tools"
BASE = "https://desktop-tools-which-may-be-useful.github.io/oray-tools"


def sha256(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("pages_root", help="checked-out gh-pages branch working tree")
    ap.add_argument("version", help="version being published (e.g. 0.1.4)")
    ap.add_argument("stage", help="directory with <label>.<ext> artifacts for this version")
    args = ap.parse_args()

    ver_dir = os.path.join(args.pages_root, "releases", args.version)
    os.makedirs(ver_dir, exist_ok=True)

    assets = {}
    for f in sorted(os.listdir(args.stage)):
        if f.startswith("."):
            continue
        stem, ext = os.path.splitext(f)
        out = f"{PROJECT}-{args.version}-{f}"
        src = os.path.join(args.stage, f)
        dst = os.path.join(ver_dir, out)
        shutil.copy2(src, dst)
        assets[stem] = {
            "url": f"{BASE}/releases/{args.version}/{out}",
            "size": os.path.getsize(dst),
            "sha256": sha256(dst),
        }

    manifest = {"project": PROJECT, "version": args.version, "assets": assets}
    with open(os.path.join(ver_dir, "manifest.json"), "w") as fh:
        json.dump(manifest, fh, indent=2)
        fh.write("\n")

    releases_path = os.path.join(args.pages_root, "releases.json")
    if os.path.exists(releases_path):
        with open(releases_path) as fh:
            releases = json.load(fh)
    else:
        releases = {"project": PROJECT, "latest": None, "versions": [], "assets": {}}

    if args.version in releases["versions"]:
        releases["versions"].remove(args.version)
    releases["versions"].insert(0, args.version)
    releases["latest"] = args.version
    releases["assets"][args.version] = assets
    with open(releases_path, "w") as fh:
        json.dump(releases, fh, indent=2)
        fh.write("\n")

    with open(os.path.join(args.pages_root, "latest.json"), "w") as fh:
        json.dump(manifest, fh, indent=2)
        fh.write("\n")

    print(f"published {args.version}: {sorted(assets)}")


if __name__ == "__main__":
    main()