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

## Looking at the web interface

`mise run web:serve` serves the real web interface at `http://localhost:8484/`
over throwaway sample data, signed in, with no mailbox or passkey needed. Every
page works, and Undo, Spam and Good change the sample data as a real action
would, so it is the way to see a template or script change in a browser. The
settings are environment variables, all optional:

- `TAMIS_MOCK_PORT` is the port (8484).
- `TAMIS_MOCK_ROWS` is how many extra decisions and whitelist addresses to
  add (150, enough to need a second page).
- `TAMIS_MOCK_FAIL`, set to anything, makes every action fail and the daemon
  report an error, to see the error states.

The sign-in page (`/auth/login`) and the enrol page (`/auth/enroll`) are served
too, without the automatic session, so you can see them and their script run;
the passkey ceremony itself has nothing to check against and fails.

Edits to `web/static` show on reload; template and Rust changes need the task
restarted. The interface only lives in the test build, so it adds nothing to the
shipped binary. `mise run web:mock` writes a static snapshot of the decisions
page from the same sample rows, for the landing page.

## Testing

Run the tests through mise rather than bare `cargo test`: the `prebuild` task
builds the web interface files first, so you test the same build CI does.

| Task | What it runs |
| --- | --- |
| `mise run test` | The Rust tests (`cargo nextest`, all features) and the browser script tests |
| `mise run test:js` | Only the browser script tests |
| `mise run test:imap` | The IMAP tests against a real Dovecot server |
| `mise run ci:test` | What CI runs: tests with coverage, the CRAP report and the browser script tests |
| `mise run full-check` | `fix`, then `ci:test`, then every `hk` check: run it before you push |

### The Rust tests

Unit tests sit next to the code in each module. Tests that go through the
public API are in `tests/`:

- `config_golden.rs`, `classify_golden.rs`, `auth_golden.rs` and
  `store_golden.rs` check the code against recorded inputs and results in
  `tests/fixtures`: configuration files (`config`), raw messages (`mail`) and
  recorded expected output and a recorded database (`golden`). A test that opens
  the recorded database works on a copy, so the fixture is never written to.
  When behaviour changes on purpose, update the recorded results in the same
  commit and say why.
- `imap_hostile.rs` runs the IMAP client against a small in-process server that
  sends what some real ones do: it must answer or fail, never hang. It needs
  nothing outside the test.

Name a test for what it shows, as a sentence
(`a_decision_offers_undo_only_when_it_can_be_undone`). A test that must
`unwrap` says so once with `#![allow(clippy::unwrap_used, reason = "...")]`
at the top of the file, as the others do.

### The IMAP tests against Dovecot

`imap_dovecot.rs` and `cli_dovecot.rs` need a real server. `mise run test:imap`
starts a Dovecot container from `tests/dovecot` (it uses Apple's `container`,
Docker or Podman, whichever it finds first), runs those two test files with
`TAMIS_TEST_IMAP` set to its address, and removes the container afterwards.
`mise run test:imap-server` leaves it running, for running tests by hand.

Without `TAMIS_TEST_IMAP` each of those tests returns at once, so a plain
`mise run test` passes without exercising a real server. Run `test:imap`
after changing anything under `src/mail`.

### The web interface

The Rust tests in `src/web/tests.rs` drive the real router in memory, with a
temporary database and a signed-in session, and check the HTML that comes back.
The helpers there do most of the work: `site()` or `site_with(Options)` builds
one, `get`, `get_htmx` and `post` send requests (a `post` carries the
same-origin header the interface demands), and `record_as` adds a decision to
look at. A new page or action needs a test in that file.

The scripts in `web/static` are tested in `web/test` with Node's built-in test
runner (`mise run test:js`). Each test loads the script into a bare `node:vm`
context with a hand-made `document`, `htmx` and `fetch`, so it runs without a
browser. Fake only what the script touches, and have the test fire the events
the script listens for.

These tests see markup and script logic, not how a page looks or feels. For
that, start the interface over sample data and use it in a browser:
[looking at the web interface](../contributing/#looking-at-the-web-interface).

### Coverage

`mise run coverage` runs the tests under `cargo llvm-cov` and fails below 90%
of lines covered. Files that can only run against a live server or network are
left out of the figure; the list is in `mise-tasks/coverage`. CI also reports
each function's CRAP score (its complexity weighed against its coverage)
against `crap-baseline.json`. A regression is a warning on the CI run, not a
failure, but a function that has got complicated and untested is worth a test.

### The filter itself

Whether a change makes the filter better or worse is not a pass or fail test.
`tamis-eval` measures that on mail you label: see
[Evaluating the filter](../how-it-works/evaluation/).
