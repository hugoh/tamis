# tamis

A second-opinion spam filter for remote mailboxes.

`tamis` connects to a mail account over IMAP (later JMAP), watches the folders
you choose, and re-checks what your provider's own filter decided: it catches
spam that slipped into the inbox, rescues good mail from the junk folder, and
learns from the corrections you make in any mail client. The training model is
inspired by SpamSieve's drone setup, but runs as a server-side daemon instead
of on a Mac running Apple Mail.

> A *tamis* is a drum sieve.

## Status

Early design. Nothing filters mail yet. See [`docs/design.md`](docs/design.md)
for the plan.

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
