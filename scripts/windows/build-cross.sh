#!/usr/bin/env bash
# Build the Windows x64 NSIS installer on macOS/Linux with MinGW.
# macOS prerequisites: brew install mingw-w64 makensis sevenzip
# Also needs Rust's x86_64-pc-windows-gnu target and Node.js/npm.
# Usage: scripts/windows/build-cross.sh [--skip-tests]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APP_DIR="$ROOT/apps/windows"
TRIPLE=x86_64-pc-windows-gnu
SKIP_TESTS=0
case "${1:-}" in
  "") ;;
  --skip-tests) SKIP_TESTS=1 ;;
  *) echo "Usage: $0 [--skip-tests]" >&2; exit 2 ;;
esac

for tool in cargo rustc node npm python3 x86_64-w64-mingw32-gcc x86_64-w64-mingw32-objdump makensis 7zz; do
  command -v "$tool" >/dev/null || { echo "Missing prerequisite: $tool" >&2; exit 1; }
done

cd "$ROOT"
cargo build --locked --release -p rustgit-cli --features iroh --target "$TRIPLE"
cd "$APP_DIR"
[[ -d node_modules ]] || npm ci
node scripts/stage-sidecar.mjs --target "$TRIPLE"
if [[ $SKIP_TESTS -eq 0 ]]; then
  npm run typecheck
  npm test
fi
npm run tauri -- build --target "$TRIPLE" --bundles nsis

VERSION="$(node -p 'require("./package.json").version')"
OUT="$ROOT/dist/windows"
mkdir -p "$OUT"
shopt -s nullglob
INSTALLERS=("$APP_DIR/src-tauri/target/$TRIPLE/release/bundle/nsis/"*-setup.exe)
[[ ${#INSTALLERS[@]} -eq 1 ]] || { echo "Expected one NSIS installer, found ${#INSTALLERS[@]}" >&2; exit 1; }
DEST="$OUT/RustGit-$VERSION-windows-x64-setup.exe"
cp "${INSTALLERS[0]}" "$DEST"
python3 "$ROOT/scripts/windows/verify-installer.py" "$DEST"
(cd "$OUT" && shasum -a 256 "$(basename "$DEST")" > SHA256SUMS)
{
  echo "RustGit $VERSION — Windows x64 desktop installer"
  echo "target: $TRIPLE"
  echo "signing: unsigned"
  echo "build: cross-compiled on $(uname -s) $(uname -m)"
  echo "verified contents: rustgit-windows.exe, rgit.exe and WebView2Loader.dll"
  echo "Native Windows install/launch verification is separate from packaging."
  cat "$OUT/SHA256SUMS"
} > "$OUT/BUILD-INFO.txt"
echo "Installer: $DEST"
