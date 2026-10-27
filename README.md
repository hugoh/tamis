# tamis

A second-opinion spam filter for remote mailboxes.

`tamis` connects to a mail account over IMAP (later JMAP), watches the folders
you choose, and re-checks what your provider's own filter decided: it catches
spam that slipped into the inbox and learns from the corrections you make in
any mail client. The training model is
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
# Run without a shell; prints the password. Or set $TAMIS_PASSWORD instead.
password_command = ["security", "find-generic-password", "-s", "tamis", "-w"]
```

Every other setting has a default:

```toml
state = "~/.local/state/tamis/tamis.db"

[account]
port = 993

[folders]
watch = ["INBOX"]
junk = "Junk"
sent = "Sent Messages"
train_spam = "TrainSpam"   # created if missing
train_good = "TrainGood"

[policy]
mode = "shadow"            # "live" to actually file mail
spam_threshold = 0.9       # score >= this is spam
good_threshold = 0.2       # score <= this is good; between is uncertain
poll_interval = "2m"
whitelist_requires_auth = true  # trust a whitelisted sender only if DKIM/DMARC passed
```

Then:

```sh
tamis bootstrap   # whitelist Sent recipients, train on Junk and old read mail
tamis run         # judge new mail every poll_interval (--once for one pass)
tamis log         # what it decided, and in shadow mode what it would have done
tamis status      # mode, folders, how much it has learnt
```

To correct it from any mail client, on any device:

- Drop a missed spam into **TrainSpam**: it is learnt and moved to Junk.
- Drop a good message into **TrainGood**: it is learnt, its sender
  whitelisted, and it is moved back to the inbox.
- In live mode, moving something tamis filed back out of Junk also teaches it
  that the message is good.

`bootstrap` trains on Junk and the inbox by default. Name other folders, and
how many recent messages to take from each, with `-s`/`--spam` and
`-g`/`--good`:

```sh
tamis bootstrap -s Junk=500 -g INBOX=500 -g Archive=300
```

Only read messages at least `--good-min-age` old (default a week) are
trained as good. `--sent N` sets how many sent messages to whitelist
recipients from (default 2000).

When `tamis log` looks right for a week or two, set `mode = "live"`.

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
