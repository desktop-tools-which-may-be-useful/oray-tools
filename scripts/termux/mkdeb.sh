#!/usr/bin/env bash
# Build a Termux-style .deb from a single executable.
#
# Usage: mkdeb.sh <binary> <version> <arch> <out.deb>
#   arch: aarch64 | arm | x86_64 | i686
set -euo pipefail

BIN=${1:?binary path}
VER=${2:?version}
ARCH=${3:?termux arch}
OUT=${4:?output .deb path}

[[ -f "$BIN" ]] || { echo "error: binary not found: $BIN" >&2; exit 1; }
INSTALLED=$(du -k "$BIN" | cut -f1)

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT

mkdir -p "$root/root/usr/bin"
cp "$BIN" "$root/root/usr/bin/oray-tools"
chmod 755 "$root/root/usr/bin/oray-tools"

cat > "$root/control" <<EOF
Package: oray-tools
Version: $VER
Architecture: $ARCH
Maintainer: desktop-tools-which-may-be-useful
Installed-Size: $INSTALLED
Depends: libc
Section: utils
Priority: optional
Description: Oray smart plug control CLI
EOF

( cd "$root" && tar --owner=0 --group=0 --numeric-owner -czf control.tar.gz ./control )
( cd "$root/root" && tar --owner=0 --group=0 --numeric-owner -czf "$root/data.tar.gz" ./usr )

printf '2.0\n' > "$root/debian-binary"
( cd "$root" && ar rcs "$OUT" debian-binary control.tar.gz data.tar.gz )

echo "built $OUT"
