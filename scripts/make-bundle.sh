#!/usr/bin/env bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${CARLITO_BINARY:-$ROOT/app/target/aarch64-unknown-linux-gnu/release/carlito}
DEST="$ROOT/dist/carlito"
test -f "$BIN"
test -f "$ROOT/quill/build/libquill.so"
mkdir -p "$DEST"
install -m 755 "$BIN" "$DEST/carlito"
install -m 755 "$ROOT/quill/build/libquill.so" "$DEST/libquill.so"
install -m 755 "$ROOT/scripts/appload-launch.sh" "$ROOT/scripts/carlito-takeover.sh" "$DEST/"
install -m 644 "$ROOT/app/external.manifest.json" "$ROOT/app/settings.schema.json" "$ROOT/app/carlito.env.example" "$DEST/"
install -m 644 "$ROOT/assets/carlito-icon.png" "$DEST/icon.png"
# Real .env files are deliberately never packaged.
echo "Bundle: $DEST"
