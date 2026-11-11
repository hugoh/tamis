---
title: Contributing
---

Tooling is managed by [mise](https://mise.jdx.dev).

```sh
mise install
hk install
mise run ci
```

The web interface embeds htmx and a stylesheet from `web/static/vendor`.
They are not committed: the stylesheet is built by the Tailwind CLI with the
daisyUI plugin, from only the classes in `templates`, so it stays
small. The tool versions are pinned in `.config/mise/conf.d/web.toml`, where
Renovate updates them, so an update is just a change to that file.

`mise run web:vendor` produces the files, and the `prebuild` task from the
template runs it before every `mise run build`, `test`, `lint:rust` and so on, so
`mise run ci` needs nothing extra. A test fails if a template uses a class the
stylesheet lacks.

A plain `cargo build` does not run it. Such a binary still works, but the web
interface refuses to start, saying to run `mise run web:vendor` and rebuild.
The version is at the foot of every page.

### Looking at the web interface

`mise run web:serve` serves the real web interface at <http://localhost:8484/>
over throwaway sample data, signed in, with no mailbox or passkey needed. Every
page works, and Undo, Spam and Good change the sample data as a real action
would, so it is the way to see a template or script change in a browser. The
settings are environment variables, all optional:

- `TAMIS_MOCK_PORT` is the port (8484).
- `TAMIS_MOCK_ROWS` is how many extra decisions and whitelist addresses to
  add (150, enough to need a second page).
- `TAMIS_MOCK_FAIL`, set to anything, makes every action fail and the daemon
  report an error, to see the error states.

Edits to `web/static` show on reload; template and Rust changes need the task
restarted. The interface only lives in the test build, so it adds nothing to the
shipped binary. `mise run web:mock` writes a static snapshot of the decisions
page from the same sample rows, for the landing page.
