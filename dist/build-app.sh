#!/bin/sh
# haboku.app を組み立てる。macOS 標準のツールだけで完結する（ADR-0015 の配布導線）。
#
#   sh dist/build-app.sh        → target/dist/haboku.app
#
# **release で焼く。** debug だと vault 読み込みの数字が一桁変わる（CLAUDE.md の Done の定義）。
# ここで作る .app は ad-hoc 署名（`codesign -s -`）なので、**この Mac でしか開けない**。
# 他人に配るには Developer ID での署名と notarization が必要（Phase 2）。
set -eu

cd "$(dirname "$0")/.."

VERSION=$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)
APP="target/dist/haboku.app"

cargo build --release

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp target/release/haboku "$APP/Contents/MacOS/haboku"
sed "s/__VERSION__/$VERSION/g" dist/Info.plist > "$APP/Contents/Info.plist"

if [ -f assets/haboku.icns ]; then
	cp assets/haboku.icns "$APP/Contents/Resources/haboku.icns"
else
	# **黙って進まない。** アイコンが無いと Dock で白紙になり、原因が分かりにくい。
	echo "警告: assets/haboku.icns が無いので Dock のアイコンは白紙になります" >&2
fi

# ad-hoc 署名。`--deep` は使わない（Apple が非推奨。同梱バイナリも無い）。
codesign --force --sign - --identifier com.github.hyuraku.haboku "$APP"
codesign --verify --strict --verbose=1 "$APP"

echo
echo "できました: $APP"
echo "  open $APP            … 起動する"
echo "  cp -R $APP /Applications/  … インストールする"
