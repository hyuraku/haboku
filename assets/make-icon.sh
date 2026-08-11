#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BUILD_DIR=$(mktemp -d "${TMPDIR:-/tmp}/haboku-icon.XXXXXX")
trap 'rm -rf "$BUILD_DIR"' EXIT HUP INT TERM

mkdir -p "$BUILD_DIR/swift-module-cache" "$BUILD_DIR/clang-module-cache"

CLANG_MODULE_CACHE_PATH="$BUILD_DIR/clang-module-cache" \
SWIFT_MODULECACHE_PATH="$BUILD_DIR/swift-module-cache" \
swiftc -O \
  -module-cache-path "$BUILD_DIR/swift-module-cache" \
  -Xcc -fmodules-cache-path="$BUILD_DIR/clang-module-cache" \
  -framework CoreGraphics \
  -framework ImageIO \
  -framework UniformTypeIdentifiers \
  "$SCRIPT_DIR/make-icon.swift" \
  -o "$BUILD_DIR/make-icon"

"$BUILD_DIR/make-icon" "$SCRIPT_DIR/icon-1024.png"

ICONSET_DIR="$BUILD_DIR/haboku.iconset"
mkdir -p "$ICONSET_DIR"

sips -z 16 16 "$SCRIPT_DIR/icon-1024.png" --out "$ICONSET_DIR/icon_16x16.png" >/dev/null
sips -z 32 32 "$SCRIPT_DIR/icon-1024.png" --out "$ICONSET_DIR/icon_16x16@2x.png" >/dev/null
sips -z 32 32 "$SCRIPT_DIR/icon-1024.png" --out "$ICONSET_DIR/icon_32x32.png" >/dev/null
sips -z 64 64 "$SCRIPT_DIR/icon-1024.png" --out "$ICONSET_DIR/icon_32x32@2x.png" >/dev/null
sips -z 128 128 "$SCRIPT_DIR/icon-1024.png" --out "$ICONSET_DIR/icon_128x128.png" >/dev/null
sips -z 256 256 "$SCRIPT_DIR/icon-1024.png" --out "$ICONSET_DIR/icon_128x128@2x.png" >/dev/null
sips -z 256 256 "$SCRIPT_DIR/icon-1024.png" --out "$ICONSET_DIR/icon_256x256.png" >/dev/null
sips -z 512 512 "$SCRIPT_DIR/icon-1024.png" --out "$ICONSET_DIR/icon_256x256@2x.png" >/dev/null
sips -z 512 512 "$SCRIPT_DIR/icon-1024.png" --out "$ICONSET_DIR/icon_512x512.png" >/dev/null
cp "$SCRIPT_DIR/icon-1024.png" "$ICONSET_DIR/icon_512x512@2x.png"

rm -f "$SCRIPT_DIR/haboku.icns"
iconutil -c icns "$ICONSET_DIR" -o "$SCRIPT_DIR/haboku.icns"
