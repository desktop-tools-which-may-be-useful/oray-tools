#!/usr/bin/env python3
"""Generate a Debian apt repository (Termux-compatible) from a pool of .debs.

Usage: mkrepo.py <repo-root>

Expects <repo-root>/pool/main/*.deb and produces:
  <repo-root>/dists/stable/main/binary-<arch>/Packages{,gz}
  <repo-root>/dists/stable/Release
Optionally writes a GPG-signed InRelease if GPG_KEY (armored private key)
is set in the environment.
"""
import argparse
import gzip
import hashlib
import io
import os
import subprocess
import shlex
import tarfile
from datetime import datetime, timezone, timedelta

SUITE = "stable"
COMPONENT = "main"


def deb_control(deb: str) -> str:
    """Extract the control file from a .deb using ar + GNU tar (auto-detects
    gzip/xz/zstd compression)."""
    for member, flags in (
        ("control.tar.gz", "-xzf - --to-stdout"),
        ("control.tar.xz", "-xJf - --to-stdout"),
        ("control.tar.zst", "--zstd -xf - --to-stdout"),
    ):
        p = subprocess.run(
            f"ar p {shlex.quote(deb)} {member} 2>/dev/null | tar {flags} ./control 2>/dev/null",
            shell=True,
            capture_output=True,
        )
        if p.stdout:
            return p.stdout.decode()
    raise RuntimeError(f"no control archive found in {deb}")


def control_field(control: str, field: str) -> str:
    for line in control.splitlines():
        if line.lower().startswith(field.lower() + ":"):
            return line.split(":", 1)[1].strip()
    return ""


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("repo", help="repository root (contains pool/)")
    ap.add_argument("--label", default="oray-tools", help="Release Label (default: oray-tools)")
    ap.add_argument("--origin", default="oray-tools", help="Release Origin (default: oray-tools)")
    ap.add_argument("--desc", default="oray-tools repository", help="Release Description")
    args = ap.parse_args()
    repo = os.path.abspath(args.repo)

    debs = sorted(
        os.path.join(root, f)
        for root, _, files in os.walk(os.path.join(repo, "pool"))
        for f in files
        if f.endswith(".deb")
    )
    if not debs:
        raise SystemExit(f"no .deb files found under {repo}/pool")

    # Group debs by architecture and write Packages files.
    by_arch: dict[str, list[str]] = {}
    for deb in debs:
        control = deb_control(deb)
        arch = control_field(control, "Architecture")
        by_arch.setdefault(arch, []).append(deb)

    for arch, files in by_arch.items():
        binary_dir = os.path.join(repo, "dists", SUITE, COMPONENT, f"binary-{arch}")
        os.makedirs(binary_dir, exist_ok=True)
        packages_path = os.path.join(binary_dir, "Packages")
        with open(packages_path, "w") as out:
            for deb in files:
                control = deb_control(deb)
                size = os.path.getsize(deb)
                sha256 = hashlib.sha256(open(deb, "rb").read()).hexdigest()
                rel = os.path.relpath(deb, repo)
                out.write(control.rstrip() + "\n")
                out.write(f"Size: {size}\n")
                out.write(f"SHA256: {sha256}\n")
                out.write(f"Filename: {rel}\n\n")
        with open(packages_path + ".gz", "wb") as f:
            f.write(gzip.compress(open(packages_path, "rb").read()))

    # Build the Release file.
    dist_dir = os.path.join(repo, "dists", SUITE)
    os.makedirs(dist_dir, exist_ok=True)
    archs = " ".join(sorted(by_arch))
    now = datetime.now(timezone.utc)
    release = (
        f"Origin: {args.origin}\n"
        f"Label: {args.label}\n"
        f"Suite: {SUITE}\n"
        f"Codename: {SUITE}\n"
        f"Version: 1.0\n"
        f"Architectures: {archs}\n"
        f"Components: {COMPONENT}\n"
        f"Date: {now.strftime('%a, %d %b %Y %H:%M:%S %z')}\n"
        f"Valid-Until: {(now + timedelta(days=730)).strftime('%a, %d %b %Y %H:%M:%S %z')}\n"
        f"Description: {args.desc}\n"
    )

    def hash_lines(algo: str) -> str:
        lines = []
        for arch in sorted(by_arch):
            for suffix in ("Packages", "Packages.gz"):
                path = os.path.join(repo, "dists", SUITE, COMPONENT, f"binary-{arch}", suffix)
                rel = os.path.join(COMPONENT, f"binary-{arch}", suffix)
                h = hashlib.new(algo, open(path, "rb").read()).hexdigest()
                size = os.path.getsize(path)
                lines.append(f" {h} {size:>10} {rel}")
        return "\n".join(lines)

    release += "MD5Sum:\n" + hash_lines("md5") + "\n"
    release += "SHA1:\n" + hash_lines("sha1") + "\n"
    release += "SHA256:\n" + hash_lines("sha256") + "\n"

    release_path = os.path.join(dist_dir, "Release")
    with open(release_path, "w") as f:
        f.write(release)
    print(f"wrote {release_path}")

    key = os.environ.get("GPG_KEY")
    if key:
        key_file = os.path.join(repo, ".gpg.key")
        with open(key_file, "w") as f:
            f.write(key)
        subprocess.run(
            f"gpg --batch --yes --import {shlex.quote(key_file)} 2>/dev/null",
            shell=True,
            check=False,
        )
        subprocess.run(
            f"gpg --batch --yes --armor --detach-sign --output - {shlex.quote(release_path)} > {shlex.quote(release_path)}.asc",
            shell=True,
            check=True,
        )
        # Clearsigned InRelease
        subprocess.run(
            f"gpg --batch --yes --clearsign --output {shlex.quote(os.path.join(dist_dir, 'InRelease'))} {shlex.quote(release_path)}",
            shell=True,
            check=True,
        )
        os.unlink(key_file)
        print("signed Release -> InRelease")


if __name__ == "__main__":
    main()
