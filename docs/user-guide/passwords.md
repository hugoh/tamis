---
title: Passwords
---

tamis looks for the password in this order:

1. `account.password_command`, when set: a command run without a shell,
   whose output is the password, e.g. `["pass", "show", "mail/icloud"]` or
   `["op", "read", "op://Personal/iCloud/tamis"]`. It is stopped after two
   minutes, and its output is kept for as long as `tamis run` is up.
2. A systemd credential named `password` (see below).
3. The system keychain, filled by `tamis login`: the macOS Keychain, the
   Secret Service (GNOME Keyring, KWallet) on Linux, or the Windows
   Credential Manager. The entry is named `tamis/HOST`, with your user name
   as the account. `tamis logout` removes it.
4. `$TAMIS_PASSWORD`. Other processes running as you can read environment
   variables, so prefer anything above.
5. Asking, when run on a terminal. `tamis run` asks once and keeps the
   answer in memory.

On a headless Linux server there is usually no keychain. Run tamis as a
systemd service with an encrypted credential instead:

```sh
systemd-ask-password -n | sudo systemd-creds encrypt --name=password - /etc/credstore.encrypted/tamis
```

```ini
# /etc/systemd/system/tamis.service
[Service]
ExecStart=/usr/local/bin/tamis run --config /etc/tamis/config.toml
LoadCredentialEncrypted=password:/etc/credstore.encrypted/tamis
DynamicUser=yes
StateDirectory=tamis
StateDirectoryMode=0700
Environment=XDG_STATE_HOME=/var/lib
```

systemd decrypts it into a private, in-memory directory only the service can
read, and passes its location in `$CREDENTIALS_DIRECTORY`.

Under systemd, tamis logs to stderr and the journal keeps it:
`journalctl -u tamis`. When systemd says its output goes to the journal
(`$JOURNAL_STREAM`), each line carries its priority and no timestamp of its
own, so `journalctl -u tamis -p warning` shows just the warnings and errors,
such as a rule naming a folder that doesn't exist.
