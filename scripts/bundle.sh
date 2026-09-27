#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Library/Developer/CommandLineTools}"
CARGO_BIN="${CARGO_BIN:-$HOME/.cargo/bin/cargo}"
"$CARGO_BIN" build --release --locked
APP="${APP:-dist/空間透鏡.app}"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/space-lens "$APP/Contents/MacOS/space-lens"
cp packaging/Info.plist "$APP/Contents/Info.plist"
if [ -f packaging/AppIcon.icns ]; then
  cp packaging/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
fi
codesign --force --deep --sign - "$APP"
printf 'App built: %s/%s\n' "$PWD" "$APP"

