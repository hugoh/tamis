---
title: tamis
---

A spam filter and Sieve mail sorter for remote mailboxes.

`tamis` connects to a mail account over IMAP (later JMAP) and watches the folders
you choose. It files good mail into folders with Sieve rules, for providers like
iCloud that have no server-side filtering to speak of. It also re-checks what
your provider's own spam filter decided: it catches spam that slipped into the
inbox, rescues mail from people you know out of the junk folder, and learns
from the corrections you make in any mail client. The
training model is inspired by SpamSieve's drone setup, but runs as a
server-side daemon instead of on a Mac running Apple Mail.

> A *tamis* is a drum sieve.

## Status

MVP: a personal Bayes filter with a whitelist built from your Sent mail,
training folders, corrections learnt from your own moves, Sieve rules that file
good mail into folders, and a shadow mode that only records what it would do.
It starts in shadow mode.

- [`docs/engine.md`](how-it-works/engine/): how mail is processed today.
- [`docs/rules.md`](how-it-works/rules/): filing good mail into folders with Sieve.
- [`docs/monitoring.md`](user-guide/monitoring/): getting alerted when tamis stops working.
- [`docs/design.md`](how-it-works/design/): where it is heading.

## License

[AGPL-3.0-only](https://github.com/hugoh/tamis/blob/main/LICENSE)
