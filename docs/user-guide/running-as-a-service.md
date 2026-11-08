---
title: Running as a service
---

`tamis run` is meant to stay up. On Linux, run it under systemd.

## The unit

```ini
# /etc/systemd/system/tamis.service
[Unit]
Description=tamis spam filter
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/tamis run --config /etc/tamis/config.toml
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
TimeoutStopSec=60
LoadCredentialEncrypted=password:/etc/credstore.encrypted/tamis
DynamicUser=yes
StateDirectory=tamis
StateDirectoryMode=0700
Environment=XDG_STATE_HOME=/var/lib

[Install]
WantedBy=multi-user.target
```

`LoadCredentialEncrypted=` hands tamis its password; see
[Passwords](../passwords/) for creating the credential, or drop the line and
use another source. `StateDirectory=` and `XDG_STATE_HOME` put the state
database in `/var/lib/tamis` (see
[Where tamis keeps things](../../reference/where-tamis-keeps-things/)).

Then:

```sh
sudo systemctl enable --now tamis
```

## Stopping

`systemctl stop` sends SIGTERM, as does Ctrl-C (SIGINT) in a terminal. tamis
finishes the message it is on, between messages and between training batches
and never inside one, and exits. Mail it didn't reach waits for the next pass.
A pass stuck on a server that has stopped answering is dropped 30 seconds after
the signal; a move cut off that way is journalled, so the next pass settles it.
Either way the database is closed cleanly.

systemd's default `TimeoutStopSec` of 90 seconds is enough. If you set it
lower, keep it above 30 seconds, or systemd will kill tamis mid-command: the
next start recovers, but the shutdown is no longer clean.

Ctrl-C on `tamis run --once` ends the pass the same way and exits normally.

## Reloading

`systemctl reload tamis` (a SIGHUP) re-reads the configuration file and the
rules without a restart. A file that no longer loads is reported in the log and
the running configuration is kept. The `[monitor]` and `[web]` settings and the
state location are read at startup: restart for those.

## Logs

tamis logs to stderr and the journal keeps it: `journalctl -u tamis`. When
systemd says its output goes to the journal (`$JOURNAL_STREAM`), each line
carries its priority and no timestamp of its own, so
`journalctl -u tamis -p warning` shows just the warnings and errors, such as a
rule naming a folder that doesn't exist.

To be told when tamis stops working, see [Monitoring](../monitoring/).
