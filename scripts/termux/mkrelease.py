#!/usr/bin/env python3
"""Generate JSON indexes that reference GitHub Releases download URLs.

Instead of storing binaries in git, every artifact lives in a GitHub
Release and these files only point at it.

Commands:

  mkrelease.py manifest --version VER --tag TAG --repo OWNER/REPO \\
      --stage STAGE_DIR --out FILE [--label NAME]

    The stage directory holds "<label>.<ext>" files (debian-amd64.deb,
    termux-aarch64.deb, windows-x86_64.exe, ...). The asset object for each
    file records its release download URL, size and sha256. The result is
    written to FILE; with --label the same content is also usable as the
    "unstable" / "latest" marker.

  mkrelease.py index --repo OWNER/REPO --releases RELEASES_JSON \\
      --out OUT_DIR [--unstable-tag unstable]

    RELEASES_JSON is the output of `gh api repos/OWNER/REPO/releases`.
    Emits releases.json (formal releases, newest first) plus, if the tag
    of the most recent formal release is known, latest.json. The JSON is
    stable and only references download URLs under
    https://github.com/OWNER/REPO/releases/download/<tag>/manifest.json .
"""
import argparse
import hashlib
import json
import os
import sys


def sha256(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def release_base(repo: str, tag: str) -> str:
    return f"https://github.com/{repo}/releases/download/{tag}"


def scan_stage(stage: str, repo: str, tag: str) -> dict:
    assets = {}
    for f in sorted(os.listdir(stage)):
        if f.startswith("."):
            continue
        label, _ext = os.path.splitext(f)
        rel = os.path.join(stage, f)
        assets[label] = {
            "filename": f,
            "url": f"{release_base(repo, tag)}/{f}",
            "size": os.path.getsize(rel),
            "sha256": sha256(rel),
        }
    return assets


def cmd_manifest(args: argparse.Namespace) -> None:
    assets = scan_stage(args.stage, args.repo, args.tag)
    manifest = {
        "project": "oray-tools",
        "version": args.version,
        "tag": args.tag,
        "assets": assets,
    }
    with open(args.out, "w") as fh:
        json.dump(manifest, fh, indent=2)
        fh.write("\n")
    if args.label:
        with open(args.label, "w") as fh:
            json.dump(manifest, fh, indent=2)
            fh.write("\n")
    print(f"wrote manifest for {args.tag} ({len(assets)} assets) -> {args.out}")


def cmd_index(args: argparse.Namespace) -> None:
    with open(args.releases) as fh:
        releases = json.load(fh)

    formal = []
    for r in releases:
        tag = r.get("tag_name", "")
        if tag == args.unstable_tag:
            continue
        formal.append(
            {
                "tag": tag,
                "version": r.get("name") or tag,
                "published_at": r.get("published_at"),
                "manifest_url": f"{release_base(args.repo, tag)}/manifest.json",
            }
        )

    versions = {
        "project": "oray-tools",
        "latest": formal[0].get("version") if formal else None,
        "versions": formal,
    }
    with open(os.path.join(args.out, "releases.json"), "w") as fh:
        json.dump(versions, fh, indent=2)
        fh.write("\n")

    if formal:
        latest = {"version": formal[0]["version"], "tag": formal[0]["tag"],
                  "manifest_url": formal[0]["manifest_url"]}
        with open(os.path.join(args.out, "latest.json"), "w") as fh:
            json.dump(latest, fh, indent=2)
            fh.write("\n")
    print(f"indexed {len(formal)} formal releases -> {args.out}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)

    p_manifest = sub.add_parser("manifest")
    p_manifest.add_argument("--version", required=True)
    p_manifest.add_argument("--tag", required=True)
    p_manifest.add_argument("--repo", required=True)
    p_manifest.add_argument("--stage", required=True)
    p_manifest.add_argument("--out", required=True)
    p_manifest.add_argument("--label", default=None)
    p_manifest.set_defaults(func=cmd_manifest)

    p_index = sub.add_parser("index")
    p_index.add_argument("--repo", required=True)
    p_index.add_argument("--releases", required=True)
    p_index.add_argument("--out", required=True)
    p_index.add_argument("--unstable-tag", default="unstable")
    p_index.set_defaults(func=cmd_index)

    args = ap.parse_args()
    try:
        args.func(args)
    except KeyError as e:
        print(f"missing argument in stage/release data: {e}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()