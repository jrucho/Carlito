#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
SDK=${RM_SDK:?Set RM_SDK to your installed Paper Pro Move SDK}
envfiles=("$SDK"/environment-setup-*)
unset LD_LIBRARY_PATH
source "${envfiles[0]}"
mkdir -p build vendor
if [ ! -f vendor/libqsgepaper.so ]; then
    if [ -f "$SDKTARGETSYSROOT/usr/lib/plugins/scenegraph/libqsgepaper.so" ]; then
        install -m 644 "$SDKTARGETSYSROOT/usr/lib/plugins/scenegraph/libqsgepaper.so" vendor/
    else
        echo "Copy libqsgepaper.so from your Move into quill/vendor/ first." >&2
        exit 1
    fi
fi
QTINC="$SDKTARGETSYSROOT/usr/include"
$CXX -fPIC -shared -O2 -Wl,-soname,libquill.so \
    -I "$QTINC" -I "$QTINC/QtCore" -I "$QTINC/QtGui" \
    src/epfb.cpp src/quill_c.cpp -L vendor -lqsgepaper -o build/libquill.so
