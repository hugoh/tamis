---
title: Where tamis keeps things
---

| What | Where |
|---|---|
| Configuration | `$XDG_CONFIG_HOME/tamis/config.toml`, by default `~/.config/tamis/config.toml`; or `--config` |
| State: what it learnt, folder positions, the decision journal, web passkeys | `$XDG_STATE_HOME/tamis/tamis.db`, by default `~/.local/state/tamis/tamis.db`; or `state` in the config |
| Password | The system keychain, never in the configuration or state (see Passwords) |

Installed from the deb or rpm and run as a service, tamis keeps things here
instead:

| What | Where |
|---|---|
| Configuration and rules | `/etc/tamis/config.toml`, `/etc/tamis/rules.sieve` (`root:tamis`, mode 0640) |
| State | `/var/lib/tamis/.local/state/tamis/tamis.db` |
| Password | a systemd credential, e.g. `/etc/tamis/password` (root only), loaded with `LoadCredential=` |
| The unit and its README | `/usr/lib/systemd/system/tamis.service`, `/usr/share/doc/tamis/README.systemd.md` |

The state database holds sender addresses and subjects, so tamis creates it
(and its directory) readable by you only. It refuses a configuration file
other users can write, since `password_command` runs whatever it names.
