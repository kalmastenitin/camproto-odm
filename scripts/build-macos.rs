#!/usr/bin/env bash

set -euo pipefail

export MACOSX_DEPLOYMENT_TARGET=11.0

ARM_TARGET="aarch64-apple-darwin"
INTEL_TARGET="x86_64-apple-darwin"

echo "==> Building ARM64"
cargo build --release --target "$ARM_TARGET"

echo "==> Building x86_64"
cargo build --release --target "$INTEL_TARGET"

echo "==> Creating application bundle"
cargo bundle \
  --release \
  --target "$ARM_TARGET" \
  --format osx

APP="target/$ARM_TARGET/release/bundle/osx/Camproto ODM.app"

ARM_BIN="target/$ARM_TARGET/release/camproto-odm"
INTEL_BIN="target/$INTEL_TARGET/release/camproto-odm"
UNIVERSAL_BIN="$APP/Contents/MacOS/camproto-odm"

echo "==> Creating universal executable"

lipo -create \
  "$ARM_BIN" \
  "$INTEL_BIN" \
  -output "$UNIVERSAL_BIN"

echo "==> Architectures:"
lipo -archs "$UNIVERSAL_BIN"

echo "==> Signing application"

codesign \
  --force \
  --deep \
  --sign - \
  "$APP"

codesign \
  --verify \
  --deep \
  --strict \
  --verbose=4 \
  "$APP"

echo "==> Creating DMG"

rm -rf dist
mkdir -p "dist/dmg-root"

ditto \
  "$APP" \
  "dist/dmg-root/Camproto ODM.app"

ln -s /Applications \
  "dist/dmg-root/Applications"

hdiutil create \
  -volname "Camproto ODM" \
  -srcfolder "dist/dmg-root" \
  -ov \
  -format UDZO \
  "dist/Camproto-ODM-macos-universal.dmg"

echo
echo "Finished:"
echo "  dist/Camproto-ODM-macos-universal.dmg"