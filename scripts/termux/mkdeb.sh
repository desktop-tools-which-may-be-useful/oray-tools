#!/usr/bin/env bash
# Build a .deb from a single executable.
#
# Usage: mkdeb.sh <binary> <version> <arch> <out.deb> [termux|debian]
#   arch:   termux -> aarch64 | arm | x86_64 | i686
#           debian -> amd64 | arm64 | armhf | i386 | riscv64
#   flavor: termux (default) or debian
set -euo pipefail

BIN=${1:?binary path}
VER=${2:?version}
ARCH=${3:?arch}
OUT=${4:?output .deb path}
FLAVOR=${5:-termux}

[[ "$FLAVOR" == termux || "$FLAVOR" == debian ]] || { echo "error: flavor must be termux or debian" >&2; exit 1; }
[[ -f "$BIN" ]] || { echo "error: binary not found: $BIN" >&2; exit 1; }
[[ "$OUT" = /* ]] || OUT="$PWD/$OUT"
mkdir -p "$(dirname "$OUT")"
INSTALLED=$(du -k "$BIN" | cut -f1)

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT

# Termux .deb data tarballs carry full paths under ./data/data/com.termux/files
# (the Termux root filesystem); dpkg maps them onto the Android app data dir.
if [[ "$FLAVOR" == termux ]]; then
  install_dir="$root/data/data/com.termux/files/usr/bin"
  data_tree="data"
else
  install_dir="$root/root/usr/bin"
  data_tree="usr"
  depends="Depends: libc6"
fi

mkdir -p "$install_dir"
cp "$BIN" "$install_dir/oray-tools"
chmod 755 "$install_dir/oray-tools"

cat > "$root/control" <<EOF
Package: oray-tools
Version: $VER
Architecture: $ARCH
Maintainer: desktop-tools-which-may-be-useful
Installed-Size: $INSTALLED
${depends:-}
Section: utils
Priority: optional
Description: Oray smart plug control CLI
EOF

( cd "$root" && tar --owner=0 --group=0 --numeric-owner -cJf control.tar.xz ./control )
if [[ "$FLAVOR" == termux ]]; then
  ( cd "$root" && tar --owner=0 --group=0 --numeric-owner -cJf data.tar.xz ./data )
else
  ( cd "$root/root" && tar --owner=0 --group=0 --numeric-owner -cJf "$root/data.tar.xz" ./usr )
fi

printf '2.0\n' > "$root/debian-binary"
( cd "$root" && ar rcs "$OUT" debian-binary control.tar.xz data.tar.xz )

echo "built $OUT"
