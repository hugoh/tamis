# Running tamis under systemd

This package installs the `tamis` unit but does not enable or start it: tamis
needs a configuration and a password first. The unit is skipped, not failed,
until `/etc/tamis/config.toml` exists.

1. Write the configuration as root, then let the `tamis` user read it and
   nobody else:

       sudo tamis init --host imap.example.org --user me@example.org --config /etc/tamis/config.toml
       sudo chown root:tamis /etc/tamis /etc/tamis/config.toml
       sudo chmod 750 /etc/tamis
       sudo chmod 640 /etc/tamis/config.toml

   Edit it for your folders. tamis starts in shadow mode: it records what it
   would do and moves nothing until `policy.mode = "live"`.

2. Give the service the mailbox password. Run `sudo systemctl edit tamis` and
   add one of:

       # a root-only file
       [Service]
       LoadCredential=password:/etc/tamis/password

       # or an encrypted credential made with systemd-creds
       [Service]
       LoadCredentialEncrypted=password:/etc/credstore.encrypted/tamis

   Other ways: `account.password_command` in the configuration, or
   `TAMIS_PASSWORD=...` in `/etc/default/tamis`. Without a password the
   service stays up, logs "no password" and retries.

3. Start it:

       sudo systemctl enable --now tamis

Day to day: `journalctl -u tamis` for the log, `sudo systemctl reload tamis`
after editing the configuration or rules, `sudo systemctl stop tamis` to stop
(tamis finishes the message it is on). State lives in `/var/lib/tamis`.

Full documentation: <https://tamis.larve.net/user-guide/running-as-a-service/>
