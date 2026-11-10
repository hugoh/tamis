---
title: Design
---

Status: partly built. The core (token Bayes, whitelist, train folders, inferred
corrections, rescue, shadow mode, undo, filing rules, the web interface) is
implemented; [engine.md](../../how-it-works/engine/) describes what the code does today. The
rest of this page is the plan around it, and says where it hasn't been built.
The iCloud probe has run: see [iCloud notes](../design/#icloud-notes).

## Goal

Give a remote mailbox a personal, self-hosted spam filter and Sieve sorter,
which also double-checks the provider's own filtering:

- connect a remote account (IMAP first, JMAP later);
- watch configurable inbox folders and junk folders;
- score each message with a personal classifier;
- flag or move messages, in both directions: catch spam the provider missed,
  rescue good mail the provider junked;
- file the good mail that remains into folders, with user-written Sieve rules
  ([rules.md](../../how-it-works/rules/));
- learn from the user's corrections, made in any mail client.

Filing is a first-class job next to spam filtering, but it runs only after a
message has been judged good, so a rule can never hide mail the filter was
unsure about.

Non-goals for now: hosting it for other people, sending mail, replacing the
provider's delivery-time checks (DNS blocklists, SPF/DKIM/DMARC), being a
general mail-rules engine (of Sieve, tamis acts only on `fileinto`), and editing
rules: they are written offline in a text editor, and tamis neither edits them
nor speaks ManageSieve ([rules.md](../../how-it-works/rules/#editing)).

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
SpamAssassin or Rspamd adds little. The value is in personal signals. The
plan is for several scorers to feed a small combiner (logistic regression)
trained on the user's own corrections. **Built today:** token Bayes, the
correspondent whitelist and the provider's Junk folder as a prior; the verdict
comes from the whitelist or the Bayes score against two thresholds, with no
combiner. The other rows are not built.

| Signal | Why |
|---|---|
| Token Bayes (Robinson/Fisher chi-square; [details](../../how-it-works/engine/#tokens-and-scoring)) | Cheap; covers headers, URLs, structure |
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
- The study used the Python SDK; a Rust client needs the HTTP API, not yet
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
does in their mail client. Three channels, all on by default and all built:

1. **Train folders**: `TrainSpam` and `TrainGood` per account. See
   [Where messages go](../../how-it-works/engine/#where-messages-go) and
   [Learn](../../how-it-works/engine/#learn).
2. **Inferred moves**: every message tamis touches is journalled and tagged
   with a `$TamisFiled` keyword, so a message filed as spam and later back in
   the Inbox is a false positive (train good), and one left in the Inbox and
   later in Junk is missed spam (train spam). See
   [Missed spam](../../how-it-works/engine/#missed-spam) and [Rescue](../../how-it-works/engine/#rescue).
3. **Client junk keywords**: `$Junk` / `$NotJunk`, plus Apple Mail's `Junk` /
   `NotJunk`. See [Junk and Not Junk marks](../../how-it-works/engine/#junk-and-not-junk-marks).

### Open question: should learned tokens age out?

Nothing ages today. Each token has one spam count and one good count, each
trained message is recorded once, and only `undo` or `learn` on a specific
message takes its counts back out. A spam message from today scores the same
ten years from now. Only the decision journal has a retention
(`policy.journal_retention`), and that doesn't touch scoring.

Reasons to age:

- Spam changes quickly. The words, domains and senders of this year's spam
  say little about next year's, and a long history of old counts makes the
  model slow to notice a new kind of junk.
- Good mail drifts too, more slowly: correspondents, signatures, formats.

Reasons not to:

- A rare token with few observations gets noisy when old counts expire, and
  a hard cutoff throws away rare but strong signals.
- The whitelist and recipient status already carry the stable part of the
  signal, so aging may matter less than it seems.

Sketch, if it earns its place:

1. Keep the counts as they are.
2. Record when each message was trained: the `trained` table stores no time
   today, so this is a schema step.
3. Weight old training down, or let `bootstrap --refresh` rebuild from only
   the last N months of the mailbox.

Decide with data, not by argument: `tamis-eval` replays a corpus oldest first,
scoring each message before learning it, so a variant that downweights old
training can be compared with today's behaviour on real mail. See
[evaluation.md](../../how-it-works/evaluation/).

## Phishing through real sending services

In November 2026 seven messages posing as "SendGrid Account Team" notices
(failing API requests, a bounce rate, a login alert, a receipt) reached the
author's inbox. Four were judged by tamis before being moved to Junk by hand:
three as good (scores 0.01, 0.01 and 0.04) and one as uncertain (0.57). Spam
scoring alone does not fit this kind of mail, and the notes below record why,
what was tried, and what would be worth trying next.

### Why the existing defences miss it

- **Authentication passes.** The messages were sent through real SendGrid
  accounts belonging to unrelated companies, with SPF, DKIM and DMARC aligned
  to the sending account's own domain. Authentication shows who sent a
  message, not whether the display name is true.
- **The display name was invisible.** Only the address was tokenized. Five
  of the seven claimed to be SendGrid in the display name; two said only
  "noreply" and put the brand in the subject.
- **The bodies read like receipts.** Scored against a model trained on the
  author's mail (2,500 good, 609 spam), three of the seven scored 0.00 and the
  rest 0.34 to 0.52. Their strongest good clues were the words of a receipt
  (months, quantities, dates), which the author's archive is full of. The one
  spam clue, `sendgrid`, was outvoted.
- **The links say nothing.** Every link goes through the sender's own
  click-tracking host, so the destination is not visible, and the plain-text
  part is a one-line stub.

### What was tried

| Idea | Result |
|---|---|
| Brand check: flag a display name that claims a brand whose domains the sender is not on (`auth::impersonated`) | Catches 5 of the 7 fixtures, none of the two "noreply" ones. Needs a list of brands, which someone must maintain. Built and tested, kept on a branch, **not merged and not wired** into the engine. [Details](../../how-it-works/engine/#judging-a-message) |
| Tokenize the display name (`name:<word>`) | Shipped, see [Tokens](../../how-it-works/engine/#tokens). Evaluation loss 1.225 to 1.130, better in 10 of 10 runs, good mail unaffected. It moved the SendGrid-named fixtures up by only 0.04 to 0.2: a gain in general, not a fix for this campaign |
| Learn which brand uses which domain from good mail | Not possible here: none of the 2,500 good messages had a SendGrid sender, so there was nothing to learn |
| Link text versus link target (as Rspamd's phishing module does) | Nothing to compare: the lures show no URL text, only buttons behind tracking links |
| Phishing feeds (OpenPhish, PhishTank) or domain lists (SURBL) | A local feed file keeps mail private but needs a refresh job, and a fresh tracking URL on a hijacked account is unlikely to be listed. A DNS list sends the checked domains to a third party |

Prior art found: SpamAssassin's `FromNameSpoof` and `Phishing` plugins,
Rspamd's phishing module (URL based) and commercial brand rules such as
Sublime's. No Rust crate does message-level phishing detection; the nearest
(`mailguard-rs`, `nettfiske`) look up domains. This was a web search, not a
review of their source.

### Possible next steps

Each has a trigger, so none is built on speculation.

- **Wire the brand check.** Trigger: a message whose display name claims a
  brand reaches the inbox after the display name tokens shipped. Sketch: a
  flagged message that is not whitelisted is judged spam (or uncertain, behind
  a setting), journalled with its own reason, not trained on, and never
  rescued; a whitelisted sender with passing authentication wins, so a false
  positive stays correctable. Open: ship a short built-in brand list or
  require one in the config. The author prefers not to maintain a list.
- **Find out why receipts score 0.00.** Trigger: any further good verdict on
  a lure. Look at the clue list of the decision before adding features.
- **A "first message from this domain" token.** Trigger: lures from senders
  never seen before keep scoring as good. Test it as a `tamis-eval` variant.
- **A local phishing feed**, off by default. Trigger: phishing that links to a
  known-bad URL.
- **Link text versus target.** Trigger: lures that show a URL whose host
  differs from the link's.

### Tracking

Add the sanitized message to `tests/fixtures/mail/` as `phish-*.eml` (replace
the recipient, drop per-recipient headers and tracking tokens, as the existing
ones are), and score it with `tamis-eval`. A resolved step moves out of this
list and into [engine.md](../../how-it-works/engine/); the Roadmap row below
points here for the rest.

## Actions and safety

Built, and described in [engine.md](../../how-it-works/engine/): [actions](../../how-it-works/engine/#actions)
(spam to Junk, uncertain flagged in place with `$TamisUncertain`, good mail
filed by [rules](../../how-it-works/rules/)), [rescue](../../how-it-works/engine/#rescue) from the provider's
Junk, [undo](../../how-it-works/engine/#undo), shadow mode for the first one to two weeks,
and the [safety rules](../../how-it-works/engine/#safety-rules): never delete, always journal,
always fetch with `BODY.PEEK`.

Not built: a `Junk/Uncertain` folder, a flag-only option for spam, and rate
limiting of moves. Actions are idempotent, so a move can be repeated after a
failure.

## Feedback

Planned, not built: a daily digest message delivered into the mailbox itself
(caught, uncertain, rescued, with undo links). What exists instead is the web
interface (decisions with undo and learn, whitelist, health, rules, logs),
`tamis log`, `tamis doctor`, and dead-man's-switch pings that alert when the
daemon stops or the filter's quality drops; see [monitoring.md](../../user-guide/monitoring/).
A revoked app-specific password shows up there too, through a channel other
than the mailbox.

## Web interface security

The interface shows untrusted data (senders, subjects, reasons taken from
mail) to a signed-in user who can undo decisions, edit the whitelist and turn
on debug logging. The design assumes an escaping mistake will happen one day
and limits what it can do. Every response carries:

```text
Content-Security-Policy: default-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'
```

- **`default-src 'self'`** covers scripts, styles, images, fonts and
  connections, so the page loads only from its own origin. There is no
  `'unsafe-inline'` and no `'unsafe-eval'`: injected markup cannot run a
  script, and it cannot restyle the page or send data to another site.
- **`frame-ancestors 'none'`** stops any site framing the interface
  (clickjacking).
- **`form-action 'self'`** stops an injected form from posting elsewhere.
- **`base-uri 'none'`** stops an injected `<base>` from redirecting relative
  URLs.

The rest of the design follows from that policy:

- **No inline code.** Templates carry no `<script>`, `style=` or `on*=`
  attributes; behaviour lives in files under `web/static` (for example
  `whitelist.js`, `rules.js`).
- **htmx runs without eval or inline styles.** It is configured in
  `layout.html` with `allowEval: false`, `allowScriptTags: false` and
  `includeIndicatorStyles: false`. The last one stops htmx injecting its
  indicator `<style>` block, which this policy would block. So
  `.htmx-indicator` does nothing here, and loading feedback is either
  `hx-disabled-elt` (it sets the `disabled` attribute, which needs no CSS) or
  a rule on the `htmx-request` class in the vendored stylesheet.
- **Nothing from another site.** htmx and the stylesheet are served from
  `web/static/vendor` (see [contributing.md](../../contributing/)), with no CDN.

Loosening the policy is not the way to get a feature. `'unsafe-inline'` in
`script-src` turns any escaping bug into code running in an authenticated
session, which the `Origin` check cannot stop because the request comes from
the interface's own origin.

The other defences:

- State-changing requests (anything but `GET` and `HEAD`) whose `Origin` is not
  the interface's own get `403`.
- `X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer`.
- `Strict-Transport-Security` when the interface is served over HTTPS.
- `Cache-Control: no-store` on everything except `/static/`.
- Passkey sign-in, with sessions that expire after 12 hours.

The header and the check are in `src/web/mod.rs`, and a test asserts the
header.

## Onboarding

[Bootstrap](../../how-it-works/engine/#bootstrap) does the seeding: the whitelist from Sent
recipients, Junk as spam, read-and-kept mail as good, then shadow mode and
going live. Server auto-discovery and a folder-confirmation step were planned
and are not built; the folders come from the configuration.

## iCloud notes

The first target account is iCloud, the most constrained of the big
providers:

- IMAP only (`imap.mail.me.com:993`), app-specific password, no OAuth, no JMAP.
- No MOVE, and CONDSTORE/QRESYNC are advertised but have been buggy: how
  tamis copes is in [imap.md](../../how-it-works/imap/#server-behaviour-tamis-relies-on) and
  [capabilities](../../how-it-works/imap/#capabilities). `tamis probe` measures what a given
  server's extensions actually do.
- Measured with `tamis probe --write` on a real account (2026-11-03): IDLE,
  UIDPLUS and custom keywords (`PERMANENTFLAGS` includes `\*`) work;
  APPENDUID is returned; `BODY.PEEK` leaves mail unread; two concurrent
  connections are accepted; an IDLE push arrives in about 4 seconds; MOVE and
  SPECIAL-USE are absent, so the Junk folder is found by name, not by
  `\Junk`; the COPY + STORE + UID EXPUNGE fallback works.
- Connection limits are undocumented: IDLE on the watched folders only, poll the
  rest ([Idle and polling](../../how-it-works/imap/#idle-and-polling)).
- Apple's server filter, Mail.app's local filter and SpamSieve all move mail
  too; tamis must tolerate other actors.
- No server-side Sieve, which is part of why this project exists. Good mail is
  filed by running a Sieve script client-side instead; see [rules.md](../../how-it-works/rules/).

## Architecture

One Rust daemon (JMAP, the combiner and the API are plans, not built):

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
| MVP (done, see [engine.md](../../how-it-works/engine/)) | IMAP, Train folders, token Bayes, whitelist from Sent, shadow mode, SQLite, CLI |
| Filing (done, see [rules.md](../../how-it-works/rules/)) | Sieve rules file good mail into folders; `tamis rules`; web Rules page |
| Corrections and rescue (done, see [engine.md](../../how-it-works/engine/)) | Inferred corrections, `$Junk`/`$NotJunk` marks, rescue from Junk, undo and learn |
| Web interface (done) | Passkey sign-in, decisions with undo and learn, whitelist, rules, health, logs |
| Next | Opt-in Jev signal and the combiner, digest, Hide My Email alias check, token aging (open question above), [phishing next steps](../../how-it-works/design/#phishing-through-real-sending-services) |
| Later | JMAP, OAuth (Gmail, Microsoft), local embeddings, optional Rspamd signal |
