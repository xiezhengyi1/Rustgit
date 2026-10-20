#!/usr/bin/env bash
# Builds RustGit.app (SwiftUI app + bundled `rgit` engine) and a .dmg / .zip.
#
#   scripts/macos/build-app.sh [options]
#     --out DIR           output directory (default: dist/macos)
#     --identity NAME     codesign identity; "-" (default) is ad-hoc signing, which is NOT
#                         notarizable and triggers Gatekeeper warnings on other Macs
#     --skip-cargo        reuse an existing target/release/rgit
#     --rgit PATH         bundle this prebuilt rgit (implies --skip-cargo)
#     --universal         also build x86_64 and lipo both slices (needs: rustup target add x86_64-apple-darwin)
#     --version X.Y.Z     CFBundleShortVersionString (default: taken from crates/cli/Cargo.toml)
#     --bundle-id ID      CFBundleIdentifier (default: app.rustgit.RustGit)
#     --no-dmg            only produce the .zip
#
# Environment: RGIT_SWIFT_SDK=/path/to/MacOSX26.x.sdk overrides the SDK choice (see below).
# Only Xcode's Command Line Tools are required; the Xcode IDE is not.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="$ROOT/dist/macos"; IDENTITY="-"; SKIP_CARGO=0; UNIVERSAL=0; DMG=1
BUNDLE_ID="app.rustgit.RustGit"; VERSION=""; PREBUILT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="$2"; shift 2;;
    --identity) IDENTITY="$2"; shift 2;;
    --skip-cargo) SKIP_CARGO=1; shift;;
    --rgit) PREBUILT="$2"; SKIP_CARGO=1; shift 2;;
    --universal) UNIVERSAL=1; shift;;
    --version) VERSION="$2"; shift 2;;
    --bundle-id) BUNDLE_ID="$2"; shift 2;;
    --no-dmg) DMG=0; shift;;
    -h|--help) sed -n '2,16p' "$0"; exit 0;;
    *) echo "unknown option $1" >&2; exit 2;;
  esac
done

[[ "$(uname)" == "Darwin" ]] || { echo "This script builds a macOS app and must run on macOS." >&2; exit 1; }
[[ -f "$HOME/.cargo/env" ]] && . "$HOME/.cargo/env"
command -v cargo >/dev/null || { echo "cargo not found (install Rust via rustup)." >&2; exit 1; }
command -v swift >/dev/null || { echo "swift not found (xcode-select --install)." >&2; exit 1; }

[[ -n "$VERSION" ]] || VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/crates/cli/Cargo.toml" | head -1)"
VERSION="${VERSION:-0.1.0}"
BUILD="$(date +%Y%m%d%H%M)"
ARCH="$(uname -m)"

# --- SDK choice ------------------------------------------------------------------------------
# The macOS 27 SDK implements SwiftUI's @State etc. as macros whose compiler plug-in ships with
# the Xcode IDE only. With just the Command Line Tools, build against the newest older SDK.
SWIFT_SDK_ARGS=()
if [[ -n "${RGIT_SWIFT_SDK:-}" ]]; then
  SWIFT_SDK_ARGS=(--sdk "$RGIT_SWIFT_SDK")
elif [[ "$(xcode-select -p)" == */CommandLineTools ]]; then
  SDK="$(ls -d /Library/Developer/CommandLineTools/SDKs/MacOSX2[0-6]*.sdk 2>/dev/null | sort -V | tail -1 || true)"
  if [[ -n "$SDK" ]]; then SWIFT_SDK_ARGS=(--sdk "$SDK"); echo "Using SDK $SDK (Command Line Tools only)"; fi
fi

# --- 1. engine --------------------------------------------------------------------------------
RGIT_ARM="${PREBUILT:-$ROOT/target/release/rgit}"
if [[ $SKIP_CARGO -eq 0 ]]; then
  echo "==> cargo build --release"
  (cd "$ROOT" && cargo build --release -p rustgit-cli --features iroh)
fi
[[ -x "$RGIT_ARM" ]] || { echo "missing $RGIT_ARM" >&2; exit 1; }
RGIT="$RGIT_ARM"
if [[ $UNIVERSAL -eq 1 ]]; then
  echo "==> cargo build --release --target x86_64-apple-darwin"
  (cd "$ROOT" && cargo build --release -p rustgit-cli --features iroh --target x86_64-apple-darwin)
  RGIT="$ROOT/target/rgit-universal"
  lipo -create "$RGIT_ARM" "$ROOT/target/x86_64-apple-darwin/release/rgit" -output "$RGIT"
fi

# --- 2. app -----------------------------------------------------------------------------------
echo "==> swift build -c release"
APP_PKG="$ROOT/apps/macos"
# SwiftPM's macro-plugin lookup is occasionally flaky with Command Line Tools only: retry once.
swift build -c release --package-path "$APP_PKG" ${SWIFT_SDK_ARGS[@]+"${SWIFT_SDK_ARGS[@]}"} \
  || swift build -c release --package-path "$APP_PKG" ${SWIFT_SDK_ARGS[@]+"${SWIFT_SDK_ARGS[@]}"}
BIN_DIR="$(swift build -c release --package-path "$APP_PKG" ${SWIFT_SDK_ARGS[@]+"${SWIFT_SDK_ARGS[@]}"} --show-bin-path)"
if [[ $UNIVERSAL -eq 1 ]]; then
  swift build -c release --package-path "$APP_PKG" --triple x86_64-apple-macosx14.0 ${SWIFT_SDK_ARGS[@]+"${SWIFT_SDK_ARGS[@]}"}
  X86_BIN="$(swift build -c release --package-path "$APP_PKG" --triple x86_64-apple-macosx14.0 ${SWIFT_SDK_ARGS[@]+"${SWIFT_SDK_ARGS[@]}"} --show-bin-path)"
  lipo -create "$BIN_DIR/RustGit" "$X86_BIN/RustGit" -output "$ROOT/target/RustGit-universal"
  APP_BIN="$ROOT/target/RustGit-universal"
else
  APP_BIN="$BIN_DIR/RustGit"
fi

# --- 3. bundle --------------------------------------------------------------------------------
APP="$OUT/RustGit.app"
echo "==> assembling $APP"
rm -rf "$APP"; mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$APP_BIN" "$APP/Contents/MacOS/RustGit"
cp "$RGIT" "$APP/Contents/MacOS/rgit"
cp "$APP_PKG/Support/RustGit.icns" "$APP/Contents/Resources/RustGit.icns"
chmod 755 "$APP/Contents/MacOS/RustGit" "$APP/Contents/MacOS/rgit"
for lproj in "$APP_PKG"/Resources/*.lproj; do cp -R "$lproj" "$APP/Contents/Resources/"; done
sed -e "s|@BUNDLE_ID@|$BUNDLE_ID|" -e "s|@VERSION@|$VERSION|" -e "s|@BUILD@|$BUILD|" \
  "$APP_PKG/Support/Info.plist.in" > "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"
plutil -lint "$APP/Contents/Info.plist"

# --- 4. sign ----------------------------------------------------------------------------------
echo "==> codesign (identity: $IDENTITY)"
if [[ "$IDENTITY" == "-" ]]; then
  SIGN_OPTS=(--force --sign - --timestamp=none)
else
  # Developer ID: hardened runtime + secure timestamp are required for notarization.
  SIGN_OPTS=(--force --sign "$IDENTITY" --timestamp --options runtime)
fi
codesign "${SIGN_OPTS[@]}" --identifier "$BUNDLE_ID.rgit" "$APP/Contents/MacOS/rgit"
codesign "${SIGN_OPTS[@]}" --entitlements "$APP_PKG/Support/RustGit.entitlements" "$APP"
codesign --verify --strict --verbose=2 "$APP"

# --- 5. package -------------------------------------------------------------------------------
ZIP="$OUT/RustGit-$VERSION-macos-$([[ $UNIVERSAL -eq 1 ]] && echo universal || echo "$ARCH").zip"
rm -f "$ZIP"; ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"
echo "==> $ZIP"
if [[ $DMG -eq 1 ]]; then
  DMGFILE="${ZIP%.zip}.dmg"; STAGE="$(mktemp -d)"
  cp -R "$APP" "$STAGE/"; ln -s /Applications "$STAGE/Applications"
  rm -f "$DMGFILE"
  hdiutil create -volname "RustGit" -srcfolder "$STAGE" -ov -format UDZO "$DMGFILE" >/dev/null
  rm -rf "$STAGE"
  [[ "$IDENTITY" == "-" ]] || codesign --force --sign "$IDENTITY" --timestamp "$DMGFILE"
  echo "==> $DMGFILE"
fi

SIGNED="ad-hoc (not notarized; Gatekeeper will warn on other Macs)"
[[ "$IDENTITY" == "-" ]] || SIGNED="Developer ID signed with hardened runtime; NOT notarized until you run notarytool (see docs/macos-app.md)"
{
  echo "RustGit $VERSION (build $BUILD)"
  echo "architectures: $(lipo -archs "$APP/Contents/MacOS/RustGit")"
  echo "signing: $SIGNED"
  (cd "$OUT" && shasum -a 256 ./*.zip ./*.dmg 2>/dev/null)
} | tee "$OUT/BUILD-INFO.txt"
