#!/bin/sh
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
systemctl is-active --quiet carlito-takeover && exit 0
systemd-run --unit=carlito-takeover --collect \
  --property="ExecStopPost=-/bin/systemctl start xochitl" \
  /bin/bash "$HERE/carlito-takeover.sh"
