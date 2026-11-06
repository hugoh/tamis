# tamis

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

## Documentation

Full documentation is at <https://tamis.larve.net/docs/>: [install](https://tamis.larve.net/docs/install/), [quick start](https://tamis.larve.net/docs/quick-start/), the [user guide](https://tamis.larve.net/docs/user-guide/filing-rules/) and [how it works](https://tamis.larve.net/docs/how-it-works/engine/). The sources are in [`docs/`](docs).

## Install

```sh
brew install hugoh/tap/tamis      # Linux; see the docs for apt, dnf and mise
```

## License

[AGPL-3.0-only](LICENSE)
