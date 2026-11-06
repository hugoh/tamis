---
title: Where tamis keeps things
---

| What | Where |
|---|---|
| Configuration | `$XDG_CONFIG_HOME/tamis/config.toml`, by default `~/.config/tamis/config.toml`; or `--config` |
| State: what it learnt, folder positions, the decision journal, web passkeys | `$XDG_STATE_HOME/tamis/tamis.db`, by default `~/.local/state/tamis/tamis.db`; or `state` in the config |
| Password | The system keychain, never in the configuration or state (see Passwords) |

The state database holds sender addresses and subjects, so tamis creates it
(and its directory) readable by you only. It refuses a configuration file
other users can write, since `password_command` runs whatever it names.
