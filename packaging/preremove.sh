#!/bin/sh
set -e
case "$1" in
remove | 0)
	if [ -d /run/systemd/system ]; then
		systemctl stop tamis.service
	fi
	;;
esac
