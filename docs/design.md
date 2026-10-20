# Design

Status: draft. Captures the initial design discussion; expect it to change once
the iCloud capability probe has run.

## Goal

Complement a mail provider's spam filtering with a personal, self-hosted
second opinion:

- connect a remote account (IMAP first, JMAP later);
- watch configurable inbox folders and junk folders;
- score each message with a personal classifier;
- flag or move messages, in both directions: catch spam the provider missed,
  rescue good mail the provider junked;
- learn from the user's corrections, made in any mail client.

Non-goals for now: hosting it for other people, sending mail, replacing the
provider's delivery-time checks (DNS blocklists, SPF/DKIM/DMARC).

## Prior art

- [isbg](https://manpages.debian.org/testing/isbg/isbg.1): SpamAssassin over
  IMAP, batch (cron) runs, teach-only mode.
- [rspamd-iscan](https://github.com/fho/rspamd-iscan): daemon that watches IMAP
  folders and feeds Rspamd. Closest existing project.
- spamfilter (Docker/Unraid): Rspamd Bayes learning from Inbox↔Junk moves and
  Train folders.
- [SpamSieve drone setup](https://c-command.com/spamsieve/help/drone-setup-and-remote):
  the UX model. `TrainSpam` / `TrainGood` folders per account.

None of these offer a whitelist from correspondents, an uncertain tier,
rescue from the provider's junk folder, shadow mode, or undo.

## Classification

The provider has already applied delivery-time checks, so re-running
SpamAssassin or Rspamd adds little. The value is in personal signals. Several
scorers feed a small combiner (logistic regression) trained on the user's own
corrections:

| Signal | Why |
|---|---|
| Token Bayes (Robinson/Fisher chi-square) | Cheap; covers headers, URLs, structure |
| Embedding nearest-neighbour | Catches reworded/new spam; learns instantly from one correction |
| Correspondent whitelist | Strongest ham signal; only counts when DKIM/DMARC passes |
| Hide My Email alias ↔ expected sender | Mail to a single-site alias from someone else is suspect |
| Provider's verdict (folder, auth headers) | Free prior |

The combined score maps to three tiers: **good**, **uncertain**, **spam**.
An LLM, if ever added, only looks at the uncertain band and is opt-in.

Embeddings come from a small local model (bge-small / e5-small / MiniLM class)
via ONNX or a local embedding server; vectors are stored with the user's
labelled messages.

## Learning and corrections

The daemon runs remotely, so training must come from what the user already
does in their mail client. Three channels, all on by default:

1. **Train folders**: `tamis/TrainSpam` and `tamis/TrainGood` per account.
   Messages dropped there are trained, then moved to Junk or back to the
   Inbox.
2. **Inferred moves**: every message tamis touches is recorded (Message-ID,
   content hash, verdict) and tagged with a `$TamisFiled` keyword.
   - Filed as spam, later back in the Inbox: false positive, train good.
   - Left in the Inbox, later in Junk: missed spam, train spam.
3. **Client junk keywords**: `$Junk` / `$NotJunk`, plus Apple Mail's
   `Junk` / `NotJunk`.

## Actions and safety

- Spam tier: move to Junk (or flag only, configurable).
- Uncertain tier: move to `Junk/Uncertain`, or flag in place.
- Rescue: whitelisted sender with DKIM/DMARC pass, or a strong good score,
  in the provider's Junk gets moved back to the Inbox (or a `Rescued` folder).
- Never delete. Every action goes into a journal and can be undone.
- Rate-limit moves; all actions are idempotent.
- **Shadow mode** for the first one to two weeks: report what would have
  happened, act on nothing.
- Always fetch with `BODY.PEEK` so scanning never marks mail as read.

## Feedback

A daily digest message delivered into the mailbox itself: caught, uncertain,
rescued, with undo links to a small web UI. Login failures (e.g. a revoked
app-specific password) alert through a separate channel such as ntfy, since
the mailbox itself is unreachable then.

## Onboarding

1. Connect: server auto-discovery plus an app-specific password.
2. Discover folders via SPECIAL-USE (`\Junk`, `\Sent`); user confirms which to
   watch.
3. Build the whitelist from Sent recipients (last ~2 years) and contacts.
4. Seed the corpus: Junk as spam; read-and-kept Inbox/Archive mail and mail
   from whitelisted senders as good. A few hundred of each, balanced.
5. Shadow mode, then go live, with an accuracy page.

## iCloud notes

The first target account is iCloud, the most constrained of the big
providers:

- IMAP only (`imap.mail.me.com:993`), app-specific password, no OAuth, no JMAP.
- CONDSTORE/QRESYNC are advertised but have been buggy; MOVE is unconfirmed.
  Every extension is optional, with a full UID/flags resync as fallback.
- Without MOVE, use COPY + `\Deleted` + `UID EXPUNGE`; never plain `EXPUNGE`
  without UIDPLUS.
- Connection limits are undocumented: IDLE on the Inbox only, poll the rest.
- Apple's server filter, Mail.app's local filter and SpamSieve all move mail
  too; tamis must tolerate other actors.
- No server-side Sieve, which is part of why this project exists.

## Architecture

One Go daemon:

```text
connectors (IMAP, JMAP) ─┐
                         ├─ sync engine ── state (SQLite)
classifier + rules ──────┤
policy (score → action) ─┤
trainer ─────────────────┤
action journal (undo) ───┘
web UI / API, TOML config
```

## Roadmap

| Phase | Scope |
|---|---|
| 0 | iCloud capability probe: which extensions actually work |
| MVP | IMAP, Train folders, token Bayes, whitelist from Sent, shadow mode, SQLite, CLI |
| Next | Inferred corrections, rescue from Junk, embeddings, digest, web UI with undo |
| Later | JMAP, OAuth (Gmail, Microsoft), optional Rspamd/LLM signals |
