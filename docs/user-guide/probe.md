---
title: Probe a server
---

`tamis probe` checks which IMAP features a server actually supports, so the
sync strategy can be chosen from evidence rather than advertised
capabilities. [docs/imap.md](../../how-it-works/imap/) describes how tamis uses the server. It asks for the password (for iCloud, an app-specific one), or
reads it from `$TAMIS_PASSWORD` or stdin.

```sh
tamis probe                # read-only, for the account in your configuration
tamis probe --write        # also APPEND, IDLE, MOVE
tamis probe --host imap.mail.me.com --user you@icloud.com   # any other account
```

`--host`, `--user` and `--port` default to the configured account, and its
`password_command` applies when you probe it. The read-only report includes the
mailbox's folder list, which is where to check the exact folder names your
[filing rules](../../user-guide/filing-rules/) use.

Read-only mode logs in, lists capabilities and folders, and examines the
inbox without changing anything. `--write` creates two top-level scratch
folders named `tamis-probe-<timestamp>`, appends a test message, exercises
`BODY.PEEK`, custom keywords, IDLE push and MOVE, then deletes the folders.
`--json` prints a report you can paste into an issue.
