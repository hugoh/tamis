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
| Jev zero-shot Choice (opt-in) | Strong from day one with no corpus; errors complement Bayes |
| Embedding nearest-neighbour (later) | Local alternative for reworded spam; learns from one correction |
| Correspondent whitelist | Strongest ham signal; only counts when DKIM/DMARC passes |
| Hide My Email alias ↔ expected sender | Mail to a single-site alias from someone else is suspect |
| Provider's verdict (folder, auth headers) | Free prior |

The combined score maps to three tiers: **good**, **uncertain**, **spam**.

### Jev

[TypeSafe's Jev](https://typesafe.ai) is a hosted model that classifies
zero-shot from written category definitions and returns a probability
distribution. The [jev-spam-eval](https://github.com/bitnovus/jev-spam-eval)
study is the evidence for using it here:

- With enriched context (link destinations, Reply-To, attachment metadata) it
  reached 98.64% on a ham/spam/phishing set, 0.23 points behind TF-IDF
  regression trained on ~4,600 labelled messages.
- Its errors complement a trained token model: a fixed 50/50 average beat
  both alone (binary: 99.22% vs 98.39% for regression).
- On later mail it held up better than older-trained regression (2024–25
  phishing recall 95.3% vs 75.3%).
- Cost is negligible at personal volumes: about 1,600 input tokens per
  message, roughly $0.00007 at the study's recorded rate.

How tamis would use it:

- **Cold start.** Before the personal corpus exists, Jev plus the whitelist
  carries the decision; the combiner shifts weight to Bayes as corrections
  accumulate.
- **Context the study lacked.** Its limits list recipient history and
  verified sender authentication as missing. tamis has both: whether the
  user has written to the sender, `Authentication-Results`, `List-Id`, and
  Hide My Email alias mapping. The Ling-Spam false positives (expected
  publisher announcements) are exactly what subscription context fixes.
- **Three-way output.** Phishing gets stricter handling than spam: never
  rescued from Junk, never whitelisted by display name.
- **Corrections don't retrain Jev.** Its weights are fixed; personal learning
  stays in Bayes, the whitelist and the combiner's weights and thresholds.

Constraints:

- It is a hosted API, so message content leaves the machine. Off by
  default; send bounded text and metadata, never attachments.
- The study used the Python SDK; a Go client needs the HTTP API, not yet
  checked.
- Pin the model version, cache verdicts per message, and calibrate its
  probabilities on the user's own corrections before trusting thresholds.
- The study is exploratory, on public corpora that may have been in
  pretraining.

Local embeddings (bge-small / e5-small / MiniLM class via ONNX) remain an
option for a fully local semantic signal, but are no longer on the critical
path.

## Learning and corrections

The daemon runs remotely, so training must come from what the user already
does in their mail client. Three channels, all on by default:

1. **Train folders**: `TrainSpam` and `TrainGood` per account (configurable).
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
- CONDSTORE/QRESYNC are advertised but have been buggy, and iCloud never
  confirms `ENABLE QRESYNC`. tamis doesn't use them: it searches
  each pass instead of tracking flag changes. go-imap v2 has no client-side
  QRESYNC either.
- MOVE is missing. `tamis probe` measures what the extensions tamis relies on
  (IDLE, UIDPLUS, MOVE, SPECIAL-USE, custom keywords) actually do.
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
| 0 | iCloud capability probe: which extensions actually work (`tamis probe`) |
| MVP (done, see [engine.md](engine.md)) | IMAP, Train folders, token Bayes, whitelist from Sent, shadow mode, SQLite, CLI |
| Next | Opt-in Jev signal, inferred corrections, rescue from Junk, digest, web UI with undo |
| Later | JMAP, OAuth (Gmail, Microsoft), local embeddings, optional Rspamd signal |
