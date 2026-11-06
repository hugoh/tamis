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

`mise run vendor-web` produces the files, and the `prebuild` task from the
template runs it before every `mise run build`, `test`, `lint-rust` and so on, so
`mise run ci` needs nothing extra. A test fails if a template uses a class the
stylesheet lacks.

A plain `cargo build` does not run it. Such a binary still works, but the web
interface refuses to start, saying to run `mise run vendor-web` and rebuild.
The version is at the foot of every page.
