---
title: Running as a service
---

`tamis run` is meant to stay up. On Linux, run it under systemd.

## The unit

The deb and rpm packages install this unit
([source](https://github.com/hugoh/tamis/blob/main/packaging/tamis.service)),
disabled:

```ini
# /usr/lib/systemd/system/tamis.service
[Unit]
Description=tamis (second-opinion spam filter for remote mailboxes)
Documentation=https://tamis.larve.net/
After=network-online.target
Wants=network-online.target
ConditionPathExists=/etc/tamis/config.toml

[Service]
User=tamis
Environment=HOME=/var/lib/tamis
EnvironmentFile=-/etc/default/tamis
StateDirectory=tamis
StateDirectoryMode=0700
ExecStart=/usr/bin/tamis run --config /etc/tamis/config.toml
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
TimeoutStopSec=60
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

It runs as the `tamis` user, whose home and state directory is
`/var/lib/tamis`. `ConditionPathExists=` means the unit is skipped, not failed,
until `/etc/tamis/config.toml` exists, so installing the package before
configuring it is harmless. It carries no password: you add that in a drop-in
(see below).

## From the deb or rpm package

The same steps are in `/usr/share/doc/tamis/README.systemd.md`; see
[Install](../../install/#what-the-packages-install) for what the package puts
where.

1. Write the configuration as root, then let the `tamis` user read it and
   nobody else:

   ```sh
   sudo tamis init --host imap.example.org --user me@example.org --config /etc/tamis/config.toml
   sudo chown root:tamis /etc/tamis /etc/tamis/config.toml
   sudo chmod 750 /etc/tamis
   sudo chmod 640 /etc/tamis/config.toml
   ```

   tamis starts in shadow mode: it records what it would do and moves nothing
   until `policy.mode = "live"`. Its closing hint about `tamis login` is for
   desktops; use the next step on a server.

   Afterwards `sudo -u tamis tamis log`, `doctor` and the other commands find
   `/etc/tamis/config.toml` on their own, as long as the user has no
   `~/.config/tamis/config.toml` of their own.

2. Give the service the mailbox password with a drop-in
   (`sudo systemctl edit tamis`):

   ```ini
   [Service]
   LoadCredential=password:/etc/tamis/password
   ```

   `/etc/tamis/password` is a file only root can read (mode 0400); systemd hands its
   contents to tamis through `$CREDENTIALS_DIRECTORY`. For an encrypted
   credential, create it as described in [Passwords](../passwords/) and use
   `LoadCredentialEncrypted=password:/etc/credstore.encrypted/tamis` instead.
   `account.password_command` in the configuration, or `TAMIS_PASSWORD` in
   `/etc/default/tamis`, work too. With no password at all the service stays up,
   logs "no password" and retries with a growing delay.

3. Start it:

   ```sh
   sudo systemctl enable --now tamis
   ```

### Example: Ansible and SOPS

To keep the password in a repository without exposing it, store it encrypted
with [SOPS](https://getsops.io) and let Ansible write the root-only file and the
drop-in:

```yaml
- name: Decrypt the tamis password
  ansible.builtin.copy:
    content: "{{ lookup('community.sops.sops', playbook_dir + '/tamis/password.sops', input_type='binary', output_type='binary') }}"
    dest: /etc/tamis/password
    mode: "0400"
    owner: root
    group: root
  no_log: true
  notify: Restart tamis

- name: Hand the password to the service
  ansible.builtin.copy:
    dest: /etc/systemd/system/tamis.service.d/credentials.conf
    content: |
      [Service]
      LoadCredential=password:/etc/tamis/password
  notify: Restart tamis
```

Next to them go the configuration (`/etc/tamis/config.toml`, `root:tamis`,
0640) and the rules (`/etc/tamis/rules.sieve`), deployed with a template and a
copy; a handler that runs `systemctl restart tamis` for the configuration and
`systemctl reload tamis` for the rules completes it. `no_log: true` keeps the
password out of Ansible's output.

## Without the package

Create a `tamis` user with home `/var/lib/tamis`, install the binary, and save
the unit above as `/etc/systemd/system/tamis.service` with `ExecStart=` pointing
at your copy of the binary (for example `/usr/local/bin/tamis`). Then follow
the same steps. `DynamicUser=yes` in place of the user also works if you set
`Environment=XDG_STATE_HOME=/var/lib`, but `tamis web enroll` then has no stable
account to run as.

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
