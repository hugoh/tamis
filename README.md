# tamis

A second-opinion spam filter for remote mailboxes.

`tamis` connects to a mail account over IMAP (later JMAP), watches the folders
you choose, and re-checks what your provider's own filter decided: it catches
spam that slipped into the inbox, rescues mail from people you know out of the
junk folder, and learns from the corrections you make in any mail client. The training model is
inspired by SpamSieve's drone setup, but runs as a server-side daemon instead
of on a Mac running Apple Mail.

> A *tamis* is a drum sieve.

## Status

MVP: a personal Bayes filter with a whitelist built from your Sent mail,
training folders, corrections learnt from your own moves, and a shadow mode
that only records what it would do. It starts in shadow mode.

- [`docs/engine.md`](docs/engine.md): how mail is processed today.
- [`docs/design.md`](docs/design.md): where it is heading.

## Quick start

Create `~/.config/tamis/config.toml`:

```toml
[account]
host = "imap.mail.me.com"  # iCloud
user = "you@icloud.com"
```

Then store the password (for iCloud, an
[app-specific password](https://support.apple.com/en-us/102654)) in the
system keychain:

```sh
tamis login
```

Every other setting has a default:

```toml
state = "~/.local/state/tamis/tamis.db"  # $XDG_STATE_HOME/tamis/tamis.db

[account]
port = 993
password_command = []      # e.g. ["pass", "show", "mail/icloud"]; see Passwords

[folders]
watch = ["INBOX"]
junk = "Junk"
sent = "Sent Messages"
train_spam = "TrainSpam"   # created if missing
train_good = "TrainGood"
rescued = ""               # where rescued mail goes; empty means the first watched folder

[policy]
mode = "shadow"            # "live" to actually file mail
spam_threshold = 0.9       # score >= this is spam
good_threshold = 0.2       # score <= this is good; between is uncertain
poll_interval = "2m"
whitelist_requires_auth = true  # trust a whitelisted sender only if DKIM/DMARC passed
learn_from_junk = true     # mail tamis let through that you move to Junk is learnt as spam
learn_from_keywords = true # Junk / Not Junk buttons that disagree with tamis are learnt
rescue = true              # move mail from verified whitelisted senders out of Junk
rescue_threshold = 0       # also rescue mail scoring at or below this, e.g. 0.05; 0 is off
```

Then:

```sh
tamis bootstrap   # whitelist Sent recipients, train on Junk and old read mail
tamis run         # judge new mail every poll_interval (--once for one pass)
tamis log         # what it decided, and in shadow mode what it would have done
tamis status      # mode, folders, how much it has learnt
tamis undo 42     # reverse decision 42 from the log
```

To correct it from any mail client, on any device:

- Drop a missed spam into **TrainSpam**: it is learnt and moved to Junk.
- Drop a good message into **TrainGood**: it is learnt, its sender
  whitelisted, and it is moved back to the inbox.
- Moving a message tamis let through into Junk teaches it that the message
  is spam, and takes its sender off the whitelist.
- In live mode, moving something tamis filed back out of Junk teaches it that
  the message is good.
- Marking a message Junk or Not Junk in your mail client teaches tamis too,
  when the mark disagrees with what tamis decided.
- Moving a message tamis rescued from Junk back into Junk teaches it that the
  message is spam.

If another program also moves mail to Junk (Mail.app's own junk filter, or
SpamSieve), set `learn_from_junk = false`: tamis can't tell its moves from
yours.

`bootstrap` trains on Junk and the inbox by default. Name other folders, and
how many recent messages to take from each, with `-s`/`--spam` and
`-g`/`--good`:

```sh
tamis bootstrap -s Junk=500 -g INBOX=500 -g Archive=300
```

Only read messages at least `--good-min-age` old (default a week) are
trained as good. `--sent N` sets how many sent messages to whitelist
recipients from (default 2000).

When `tamis log` looks right for a week or two, set `mode = "live"`. If tamis
then files something it shouldn't have, `tamis undo ID` moves it back, learns
it as good and whitelists the sender.

## Where tamis keeps things

| What | Where |
|---|---|
| Configuration | `$XDG_CONFIG_HOME/tamis/config.toml`, by default `~/.config/tamis/config.toml`; or `--config` |
| State: what it learnt, folder positions, the decision journal | `$XDG_STATE_HOME/tamis/tamis.db`, by default `~/.local/state/tamis/tamis.db`; or `state` in the config |
| Password | The system keychain, never in the configuration or state (see Passwords) |

The state database holds sender addresses and subjects, so tamis creates it
(and its directory) readable by you only. It refuses a configuration file
other users can write, since `password_command` runs whatever it names.

## Passwords

tamis looks for the password in this order:

1. `account.password_command`, when set: a command run without a shell,
   whose output is the password, e.g. `["pass", "show", "mail/icloud"]` or
   `["op", "read", "op://Personal/iCloud/tamis"]`.
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

## Probe a server

`tamis probe` checks which IMAP features a server actually supports, so the
sync strategy can be chosen from evidence rather than advertised
capabilities. It asks for the password (for iCloud, an app-specific one), or
reads it from `$TAMIS_PASSWORD` or stdin.

```sh
go run . probe --host imap.mail.me.com --user you@icloud.com          # read-only
go run . probe --host imap.mail.me.com --user you@icloud.com --write  # also APPEND, IDLE, MOVE
```

Read-only mode logs in, lists capabilities and folders, and examines the
inbox without changing anything. `--write` creates two top-level scratch
folders named `tamis-probe-<timestamp>`, appends a test message, exercises
`BODY.PEEK`, custom keywords, IDLE push and MOVE, then deletes the folders.
`--json` prints a report you can paste into an issue.

## Development

Tooling is managed by [mise](https://mise.jdx.dev) and the shared
[`hugoh/go-tools`](https://github.com/hugoh/go-tools) Copier template.

```sh
mise install
hk install
mise run ci
```

## License

[MIT](LICENSE)
