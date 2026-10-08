#!/bin/bash
HERE=$(cd "$(dirname "$0")" && pwd)
restore() {
    rm -f /tmp/epframebuffer.lock
    systemctl start xochitl
}
trap restore EXIT
trap 'exit 143' INT TERM
if [ -f "$HERE/carlito.env" ]; then
    set -a
    . "$HERE/carlito.env"
    set +a
fi
export CARLITO_MODE=${CARLITO_MODE:-auto}
export CARLITO_WEB_SEARCH=${CARLITO_WEB_SEARCH:-1}
systemctl stop xochitl
rm -f /tmp/epframebuffer.lock
sleep 1
cd "$HERE"
LD_LIBRARY_PATH="$HERE:/usr/lib/plugins/scenegraph" HOME=/home/root \
    env -u QTFB_KEY "$HERE/carlito"
