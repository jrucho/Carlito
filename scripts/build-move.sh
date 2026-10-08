#!/usr/bin/env bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SDK=${RM_SDK:?Set RM_SDK to your installed Paper Pro Move SDK}
envfiles=("$SDK"/environment-setup-*)
unset LD_LIBRARY_PATH
source "${envfiles[0]}"
if [ ! -f "$ROOT/quill/build/libquill.so" ]; then
    "$ROOT/quill/build.sh"
fi
WRAPPER=$(mktemp)
trap 'rm -f "$WRAPPER"' EXIT
printf '#!/bin/sh\nexec %s "$@"\n' "$CC" > "$WRAPPER"
chmod +x "$WRAPPER"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$WRAPPER"
cargo build --manifest-path "$ROOT/app/Cargo.toml" --release \
    --target aarch64-unknown-linux-gnu --features takeover --bin carlito
"$ROOT/scripts/make-bundle.sh"
