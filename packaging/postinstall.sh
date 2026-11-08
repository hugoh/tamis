#!/bin/sh
set -e
command -v systemd-sysusers >/dev/null && systemd-sysusers tamis.conf
if [ -d /run/systemd/system ]; then
	systemctl daemon-reload
	systemctl try-restart tamis.service
fi
