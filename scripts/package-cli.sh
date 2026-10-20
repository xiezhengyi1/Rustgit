#!/bin/sh
# Builds rgit for macOS (arm64, x86_64) and Windows (x86_64, GNU toolchain, cross-compiled)
# and writes archives plus SHA256SUMS to dist/. Unsigned.
#
# Prerequisites: rustup targets aarch64-apple-darwin, x86_64-apple-darwin,
# x86_64-pc-windows-gnu; Homebrew mingw-w64 (x86_64-w64-mingw32-gcc).
set -eu
cd "$(dirname "$0")/.."
. "$HOME/.cargo/env" 2>/dev/null || true
VERSION=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print([p for p in json.load(sys.stdin)["packages"] if p["name"]=="rustgit-cli"][0]["version"])')
DIST=dist
rm -rf "$DIST"; mkdir -p "$DIST"

build() { # target
  case "$1" in
    x86_64-pc-windows-gnu)
      CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar \
        cargo build --release --locked --target "$1" -p rustgit-cli --features iroh ;;
    *) cargo build --release --locked --target "$1" -p rustgit-cli --features iroh ;;
  esac
}

stage() { # target exe-name archive-kind
  name="rgit-$VERSION-$1"
  d="$DIST/$name"; mkdir -p "$d"
  cp "target/$1/release/$2" "$d/"
  cp README.md "$d/README.md"
  cat > "$d/INSTALL.txt" <<TXT
rgit $VERSION ($1) - unsigned build.
Put $2 on your PATH (or run it from this folder) and run: $2 --help
TXT
  if [ "$3" = zip ]; then (cd "$DIST" && zip -qr "$name.zip" "$name"); else (cd "$DIST" && tar -czf "$name.tar.gz" "$name"); fi
  rm -rf "$d"
}

for t in aarch64-apple-darwin x86_64-apple-darwin; do build $t; stage $t rgit tar; done
build x86_64-pc-windows-gnu; stage x86_64-pc-windows-gnu rgit.exe zip
(cd "$DIST" && shasum -a 256 rgit-* > SHA256SUMS)
echo "wrote $DIST:"; ls -la "$DIST"
