# IMAP interface

How tamis talks to the server. The code is `internal/mail` (the client) and
`internal/engine` (what it asks for); `tamis probe` measures what a given server
actually supports (see the README). Run any command with `--verbose` to log each
exchange; credentials and message contents are never logged.

## Connections

| Connection | Lives | Does |
|------------|-------|------|
| Pass | One pass (`withEngine`), at most 10 minutes; dropped if the server stops answering | Every command below except IDLE |
| Watcher, one per watched folder | As long as the server keeps it, started after the first good pass | `SELECT` the folder, then `IDLE` |

A connection is TLS to `account.host` (port 993 by default) followed by `LOGIN`.
A rejected login ends the daemon, and a watcher that is refused gives up rather
than retry, so a wrong password isn't tried repeatedly. A watcher whose connection
fails is retried after a minute; the poll every `policy.poll_interval` covers the
gap. A server that limits concurrent sessions may refuse the watchers; the
polling pass still runs.

## Capabilities

| Capability | Use | Without it |
|------------|-----|------------|
| `IDLE` | Watchers hear of new mail and flag changes at once | The daemon polls every `poll_interval` |
| `MOVE` or `UIDPLUS` | Filing, rescuing, training | Moves are refused (`ErrUnsafeMove`): a plain `EXPUNGE` could purge other `\Deleted` mail. Without `MOVE`, tamis does `COPY`, `STORE +FLAGS \Deleted`, `UID EXPUNGE` |
| `CONDSTORE` | Learn from Junk marks by searching only flags changed since the last pass | Every pass searches all recent mail for marks |
| Custom keywords (`$Tamis…`) | Remember what tamis did | Corrections aren't learnt; the training folders still work |

## Commands

| Purpose | Commands | Notes |
|---------|----------|-------|
| Folder state | `STATUS (UIDNEXT UIDVALIDITY)` | A new `UIDVALIDITY` restarts the folder from its current mail |
| New mail | `UID SEARCH UID n:*`, then `UID FETCH` in batches of 50 | Skipped when the cursor is already at `UIDNEXT-1` |
| Fetch | `UID FETCH (UID FLAGS ENVELOPE BODY.PEEK[]<0.262144>)` | `PEEK`, so mail isn't marked read; at most 256 KiB per message |
| Mark | `UID STORE +FLAGS.SILENT` / `-FLAGS.SILENT` | |
| File | `UID MOVE` (or the fallback above) | |
| Training folders | `STATUS (MESSAGES)`; if non-empty `LIST`, `CREATE` if missing, `SELECT`, `UID SEARCH`, `FETCH`, `STORE`, `MOVE` | An empty folder costs one `STATUS` |
| Junk marks | `STATUS (UIDVALIDITY HIGHESTMODSEQ)`, then per label `UID SEARCH OR FLAG … SINCE … MODSEQ n` | Skipped when `HIGHESTMODSEQ` hasn't moved |
| Sent | Like new mail | Recipients are whitelisted |
| Undo, bootstrap | `UID SEARCH HEADER Message-ID`, `UID SEARCH SEEN BEFORE …` | On request, not every pass |
| Watching | `SELECT`, `IDLE` | Reacts to `EXISTS` and to unsolicited `FETCH` (flag changes) |

Keywords tamis sets: `$TamisFiled`, `$TamisUncertain`, `$TamisRescued`,
`$TamisTrained`. It reads `$Junk`/`Junk` and `$NotJunk`/`NotJunk` set by mail
clients.

## What a pass costs

A pass with nothing new does one `STATUS` per train folder, watched folder, Sent
and Junk, plus one `STATUS` per watched folder and Junk for `HIGHESTMODSEQ` when
`CONDSTORE` is there, and one login. Only a folder that changed gets a `SELECT`,
search or fetch.

## Idle and polling

The daemon waits for the watchers to report a change or for `poll_interval`,
whichever is first, then runs a pass. The go-imap client renews each `IDLE` every
28 minutes on the same connection, inside the 29-minute limit of RFC 2177. A pass
that files mail changes flags and moves mail the watchers can see, so one extra
pass may follow it; that pass finds nothing and stops the chain.

`poll_interval` is therefore the longest a mark made in Junk, a message dropped
in a training folder, or mail the watchers missed waits to be seen.

## Server behaviour tamis relies on

- UIDs only; message sequence numbers are never used.
- Keywords survive `MOVE`. Servers that drop them lose correction learning.
- `Message-ID` search is a substring match, and an empty ID matches nothing.
- Other actors (a server filter, a mail client's rules) move mail too; tamis
  treats anything it didn't file as theirs.
- iCloud advertises `CONDSTORE`/`QRESYNC` but has been unreliable with them and
  lacks `MOVE`; see [design.md](design.md).
