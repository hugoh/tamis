---
title: Quick start
---

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
tamis monitor     # the healthchecks.io checks to set up for [monitor], and when they alert
tamis doctor      # the configuration, the rules, the daemon, the filter and its database: are they in good shape? (--json; exits 1 on failure; same report as the web Health page)
tamis undo 42     # reverse decision 42 from the log
tamis learn 42    # learn the opposite of decision 42's verdict (shadow mode, or to change a correction)
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
