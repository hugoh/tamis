---
title: Web interface
---

`tamis run` can also serve a small web interface: the decision log with undo
and learn buttons, health, the whitelist (view and edit), the filing rules
(view, validate and try a message) and the daemon's recent log lines. It signs in with passkeys only; there are no passwords.
Everything it does has a command-line counterpart:

| In the browser | On the command line |
|---|---|
| Decisions, with Undo / Spam / Good | `tamis log`, `tamis undo`, `tamis learn` |
| Health, including the daemon's failures and recoveries | `tamis doctor`, the web Health page |
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
  tamis refuses to start unless `origin` is `https://`. Sign-in has no rate
  limit of its own: if the port is reachable from the internet, limit `/auth/`
  requests in the proxy.
- **Over an SSH tunnel**, with nothing else set up: use
  `origin = "http://localhost:8484"`, run `ssh -L 8484:127.0.0.1:8484 yourserver`
  on the machine with the browser, and browse `http://localhost:8484`.

The origin's host name is what a passkey is tied to: after changing it, enrol
again.

## HTTPS from tamis itself

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
The Logs page keeps the last 1000 lines.
Its **Turn on** button switches the daemon to debug logging for 15 minutes
(then back by itself), without a restart; the debug lines go to the journal
too. Under `--verbose` debug is on for good and the button is replaced by a note.

The interface sends a strict Content-Security-Policy, refuses state-changing
requests whose `Origin` is not yours, and serves its own copies of
[htmx](https://htmx.org) and a [daisyUI](https://daisyui.com) /
[Tailwind](https://tailwindcss.com) stylesheet: nothing is loaded from another
site.

Undo, Spam and Good show "Working…" while they run, then a toast with what
happened or why it failed, and the decision list reloads to show the new entry.
If the request never reaches tamis (the session expired, or the connection
dropped), the toast says to reload the page.

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
