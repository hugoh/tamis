# tamis

A second-opinion spam filter and mail sorter for remote mailboxes.

`tamis` connects to a mail account over IMAP (later JMAP), watches the folders
you choose, and re-checks what your provider's own filter decided: it catches
spam that slipped into the inbox, rescues mail from people you know out of the
junk folder, and learns from the corrections you make in any mail client. It
can also file the good mail that remains into folders with Sieve rules, for
providers like iCloud that have no server-side filtering to speak of. The
training model is inspired by SpamSieve's drone setup, but runs as a
server-side daemon instead of on a Mac running Apple Mail.

> A *tamis* is a drum sieve.

## Status

MVP: a personal Bayes filter with a whitelist built from your Sent mail,
training folders, corrections learnt from your own moves, Sieve rules that file
good mail into folders, and a shadow mode that only records what it would do.
It starts in shadow mode.

- [`docs/engine.md`](docs/engine.md): how mail is processed today.
- [`docs/rules.md`](docs/rules.md): filing good mail into folders with Sieve.
- [`docs/design.md`](docs/design.md): where it is heading.

## Quick start

Create `~/.config/tamis/config.toml` with `tamis init`:

```sh
tamis init --host imap.mail.me.com --user you@icloud.com  # iCloud
```

It refuses to overwrite an existing file. Or write it by hand:

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
auth_server_id = ""        # your provider's Authentication-Results domain, e.g. "icloud.com"; empty uses what bootstrap learnt, "none" turns it off

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
learn_provider_junk = true # mail the provider files to Junk that tamis isn't sure is spam is learnt
learn_from_keywords = true # Junk / Not Junk buttons that disagree with tamis are learnt
rescue = true              # move mail from verified whitelisted senders out of Junk
journal_retention = "8760h" # how long decisions are kept (a year); "0s" keeps them forever
rescue_threshold = 0       # also rescue mail scoring at or below this, e.g. 0.05; 0 is off

[rules]
script = ""                # Sieve file that files good mail into folders, e.g. "~/.config/tamis/rules.sieve"; empty is off
create_folders = false     # true: create a folder a rule names when it is missing
```

Then:

```sh
tamis bootstrap   # whitelist Sent recipients, train on Junk and old read mail
tamis run         # judge new mail as it arrives (IDLE), or every poll_interval (--once for one pass)
tamis log         # what it decided, and in shadow mode what it would have done
tamis status      # mode, folders, how much it has learnt
tamis doctor      # does the config load, do the rules compile, is the filter in good shape? (--json; exits 1 on failure)
tamis undo 42     # reverse decision 42 from the log
tamis learn 42    # shadow mode: learn the opposite of decision 42's verdict
tamis whitelist list   # also: add ADDRESS..., rm ADDRESS... or rm --matching TEXT
tamis rules show       # also: check [--folders] [FILE] to validate a Sieve script, test MESSAGE.eml to see where it goes
tamis web enroll       # one-time link to enrol a passkey for the web interface
```

`tamis log` colours its output on a terminal. `--color never` (or setting
`NO_COLOR`) turns that off; `--color always` forces it, for a pager.

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

Tamis also learns from mail your provider files to Junk, with no action from
you, when it scores below `spam_threshold` and isn't rescued. Mail it is
already sure about teaches it nothing, and the sender's whitelist entry is
left alone.

If another program also moves mail to Junk (Mail.app's own junk filter, or
SpamSieve), set `learn_from_junk = false`: tamis can't tell its moves from
yours. Set `learn_provider_junk = false` too if you don't trust that program's
verdicts as training data.

`bootstrap` trains on Junk and the inbox by default. Name other folders, and
how many recent messages to take from each, with `-s`/`--spam` and
`-g`/`--good`:

```sh
tamis bootstrap -s Junk=500 -g INBOX=500 -g Archive=300
```

Naming any of `-s`, `-g` or `--sent` limits bootstrap to what you named: `-g
INBOX=500` alone trains only from the inbox, and leaves the whitelist, Junk and
the watch position alone. Run with no flags for the full first-time setup.

`tamis bootstrap --refresh` learns mail that was already learnt again, with the
current tokenizer: run it after an upgrade that adds features, so what was
learnt before gets them too. It leaves the whitelist and the watch position
alone; name folders with `-s` and `-g` to limit it.

Only read messages at least `--good-min-age` old (default a week) are
trained as good. `--sent N` sets how many sent messages to whitelist
recipients from (default 2000).

When `tamis log` looks right for a week or two, set `mode = "live"`. If tamis
then files something it shouldn't have, `tamis undo ID` moves it back, learns
it as good and whitelists the sender.

## Filing rules

Besides catching spam, tamis can file mail it judges good into folders, with a
[Sieve](https://www.rfc-editor.org/rfc/rfc5228) script. iCloud has no
server-side Sieve, so tamis runs the script itself. Write it, for example, as
`~/.config/tamis/rules.sieve`:

```sieve
require ["fileinto", "body"];

if header :contains "list-id" "golang-nuts" {
    fileinto "Lists/Go";
    stop;
}
if address :is "from" "billing@shop.example" {
    fileinto "Receipts";
}
```

Name it in the configuration:

```toml
[rules]
script = "~/.config/tamis/rules.sieve"
```

Check it before relying on it:

```sh
tamis rules check            # does the script compile?
tamis rules check --folders  # and do the folders it files into exist?
tamis rules test some.eml    # which folder would this saved message go to?
```

The first `fileinto` that matches wins; mail that matches nothing stays in the
inbox. Rules only see mail tamis judges good, so they never file spam or mail
it is unsure about, and they only apply to mail that arrives after tamis
starts. A rule naming a folder that doesn't exist is skipped, with a warning,
so a typo doesn't make a new folder, and `tamis run` warns about each missing
folder when it starts: create your folders first, or set `create_folders =
true` under `[rules]`. The script is compiled when tamis starts, and a script with an error
stops it: restart `tamis run` after editing. In shadow mode a rule only
records what it would do (`tamis log` shows it); in live mode it moves the
message and creates the folder if needed. `tamis undo` doesn't cover rule-filed
mail: move it back by hand, and tamis won't file it again.
[`docs/rules.md`](docs/rules.md) has the details and limits.

## Web interface

`tamis run` can also serve a small web interface: the decision log with undo
and learn buttons, health, the whitelist (view and edit), the filing rules
(view, validate and try a message) and the daemon's recent log lines. It signs in with passkeys only; there are no passwords.
Everything it does has a command-line counterpart:

| In the browser | On the command line |
|---|---|
| Decisions, with Undo / Spam / Good | `tamis log`, `tamis undo`, `tamis learn` |
| Health | `tamis doctor`, `tamis status` |
| Whitelist | `tamis whitelist list`, `add`, `rm` (also `rm --matching TEXT`) |
| Rules | `tamis rules show`, `check`, `test` |
| Passkeys | `tamis web enroll`, `web passkeys`, `web revoke` |
| The certificate to trust | `tamis web cert` |
| Logs, live daemon state | the daemon's stderr (the browser view is web-only) |

Turn it on in the configuration:

```toml
[web]
enabled = true
listen = "127.0.0.1:8484"             # the default
origin = "https://tamis.example.net"  # the URL you will type in the browser
```

Then restart `tamis run` and enrol your first passkey:

```sh
tamis web enroll   # prints a link that works once, for 15 minutes
```

Open the link in the browser you will use. Run `tamis web enroll` again to add
another device.

Browsers only allow passkeys on `https://` pages and on `localhost`, and by
default the server speaks plain HTTP. So either:

- **Serve HTTPS from tamis itself**, with no proxy (see below).
- **Behind a TLS reverse proxy** (Caddy, nginx, `tailscale serve`...): proxy
  your `origin` to `listen`, which can stay on loopback. The proxy must pass the
  browser's `Origin` header through. If `listen` is not a loopback address,
  tamis refuses to start unless `origin` is `https://`.
- **Over an SSH tunnel**, with nothing else set up: use
  `origin = "http://localhost:8484"`, run `ssh -L 8484:127.0.0.1:8484 yourserver`
  on the machine with the browser, and browse `http://localhost:8484`.

The origin's host name is what a passkey is tied to: after changing it, enrol
again.

### HTTPS from tamis itself

```toml
[web]
enabled = true
listen = "0.0.0.0:8484"
origin = "https://minipc.example.net:8484"
self_signed = true   # or: tls_cert = "/path/cert.pem" and tls_key = "/path/key.pem"
```

With `self_signed`, tamis makes a certificate for the origin's host the first
time it is needed and keeps it next to the state database. It is read at start,
and made again (so it must be trusted again) when the host changes or less than
a month of its 800 days remain.

Browsers only accept passkeys from a certificate they **trust**: clicking
through a warning is not enough, and Chrome refuses outright. Install the
certificate on each device:

```sh
tamis web cert         # its fingerprint, to check what you installed
tamis web cert --pem   # the certificate itself (never the key); save it as tamis.pem
```

- **macOS (Safari, Chrome):** open `tamis.pem`, then in Keychain Access set
  Trust → "When using this certificate" → Always Trust. Or:
  `sudo security add-trusted-cert -d -r trustRoot -p ssl -k /Library/Keychains/System.keychain tamis.pem`
- **Firefox** keeps its own store: import the file under Settings → Certificates
  → Authorities, or set `security.enterprise_roots.enabled` to use the system's.
- **iOS:** install the file as a profile, then enable it under Settings →
  General → About → Certificate Trust Settings.

A certificate from a real CA, or one made by a tool such as
[mkcert](https://github.com/FiloSottile/mkcert), works the same way through
`tls_cert` and `tls_key`; tamis does not reload them, so restart after renewing.

`tamis doctor` reports whether the interface is listening and how many passkeys
are enrolled. Passkeys live in the state database. Sessions and the log lines
shown in the browser live in memory, so a restart signs you out and clears them.

The interface sends a strict Content-Security-Policy, refuses state-changing
requests whose `Origin` is not yours, and serves its own copies of
[htmx](https://htmx.org) and a [daisyUI](https://daisyui.com) /
[Tailwind](https://tailwindcss.com) stylesheet: nothing is loaded from another
site.

The decision journal and the whitelist can be large, so their tables are paged
on the server: rows load as you scroll, columns sort when you click their
headers, and the search box and the verdict buttons filter the whole journal,
not just what is on screen. The address bar follows, so a view can be
bookmarked.

To remove addresses from the whitelist, tick them (the header box ticks every
row loaded so far) and choose "Remove selected…", or choose "Remove all…" to
take out everything the current search matches, including rows not loaded yet.
Either way a dialog lists exactly what will go, and nothing is removed until you
confirm; if the matches changed in the meantime, tamis refuses and removes
nothing. On the command line, `tamis whitelist rm --matching TEXT` lists the
matches and does nothing more until you repeat it with `--yes`.

## Where tamis keeps things

| What | Where |
|---|---|
| Configuration | `$XDG_CONFIG_HOME/tamis/config.toml`, by default `~/.config/tamis/config.toml`; or `--config` |
| State: what it learnt, folder positions, the decision journal, web passkeys | `$XDG_STATE_HOME/tamis/tamis.db`, by default `~/.local/state/tamis/tamis.db`; or `state` in the config |
| Password | The system keychain, never in the configuration or state (see Passwords) |

The state database holds sender addresses and subjects, so tamis creates it
(and its directory) readable by you only. It refuses a configuration file
other users can write, since `password_command` runs whatever it names.

## Passwords

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

## Probe a server

`tamis probe` checks which IMAP features a server actually supports, so the
sync strategy can be chosen from evidence rather than advertised
capabilities. [docs/imap.md](docs/imap.md) describes how tamis uses the server. It asks for the password (for iCloud, an app-specific one), or
reads it from `$TAMIS_PASSWORD` or stdin.

```sh
tamis probe                # read-only, for the account in your configuration
tamis probe --write        # also APPEND, IDLE, MOVE
tamis probe --host imap.mail.me.com --user you@icloud.com   # any other account
```

`--host`, `--user` and `--port` default to the configured account, and its
`password_command` applies when you probe it. The read-only report includes the
mailbox's folder list, which is where to check the exact folder names your
[filing rules](#filing-rules) use.

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

The web interface embeds htmx and a stylesheet from `internal/web/static/vendor`.
They are not committed: the stylesheet is built by the Tailwind CLI with the
daisyUI plugin, from only the classes in `internal/web/templates`, so it stays
small. The tool versions are pinned in `.config/mise/conf.d/web.toml`, where
Renovate updates them, so an update is just a change to that file.

`mise run vendor-web` produces the files, and the `prebuild` task from the
template runs it before every `mise run build`, `test`, `lint-go` and so on, so
`mise run ci` needs nothing extra. A test fails if a template uses a class the
stylesheet lacks.

A plain `go build` or `go install` does not run it. Such a binary still works,
but `tamis version` shows a `+noweb` suffix and the web interface refuses to
start, saying to run `mise run vendor-web` and rebuild. A full build shows the
plain version, which is also at the foot of every page.

## License

[MIT](LICENSE)
