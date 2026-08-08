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
[[ "$OUT" = /* ]] || OUT="$PWD/$OUT"
mkdir -p "$(dirname "$OUT")"
INSTALLED=$(du -k "$BIN" | cut -f1)

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT

# Termux .deb data tarballs carry full paths under ./data/data/com.termux/files
# (the Termux root filesystem); dpkg maps them onto the Android app data dir.
mkdir -p "$root/data/data/com.termux/files/usr/bin"
cp "$BIN" "$root/data/data/com.termux/files/usr/bin/oray-tools"
chmod 755 "$root/data/data/com.termux/files/usr/bin/oray-tools"

cat > "$root/control" <<EOF
Package: oray-tools
Version: $VER
Architecture: $ARCH
Maintainer: desktop-tools-which-may-be-useful
Installed-Size: $INSTALLED
Section: utils
Priority: optional
Description: Oray smart plug control CLI
EOF

( cd "$root" && tar --owner=0 --group=0 --numeric-owner -cJf control.tar.xz ./control )
( cd "$root" && tar --owner=0 --group=0 --numeric-owner -cJf data.tar.xz ./data )

printf '2.0\n' > "$root/debian-binary"
( cd "$root" && ar rcs "$OUT" debian-binary control.tar.xz data.tar.xz )

echo "built $OUT"
