---
title: Filing rules
---

Besides catching spam, tamis can file mail it judges good into folders, with a
[Sieve](https://www.rfc-editor.org/rfc/rfc5228) script. iCloud has no
server-side Sieve, so tamis runs the script itself. Write it, for example, as
`~/.config/tamis/rules.sieve`:

```sieve
require ["fileinto", "body"];

if header :contains "list-id" "golang-nuts" {
    fileinto "Lists/Go";
    stop;
}
if address :is "from" "billing@shop.example" {
    fileinto "Receipts";
}
```

Name it in the configuration:

```toml
[rules]
script = "~/.config/tamis/rules.sieve"
```

Check it before relying on it:

```sh
tamis rules check            # does the script compile?
tamis rules check --folders  # and do the folders it files into exist?
tamis rules test some.eml    # which folder would this saved message go to?
```

The first `fileinto` that matches wins; mail that matches nothing stays in the
inbox. Rules only see mail tamis judges good, so they never file spam or mail
it is unsure about, and they only apply to mail that arrives after tamis
starts. A rule naming a folder that doesn't exist is skipped, with a warning,
so a typo doesn't make a new folder, and `tamis run` warns about each missing
folder when it starts: create your folders first, or set `create_folders =
true` under `[rules]`. The script is compiled when tamis starts, and a script with an error
stops it: restart `tamis run` after editing. In shadow mode a rule only
records what it would do (`tamis log` shows it); in live mode it moves the
message and creates the folder if needed. `tamis undo` doesn't cover rule-filed
mail: move it back by hand, and tamis won't file it again.
[`docs/rules.md`](../../how-it-works/rules/) has the details and limits.

## Supported extensions

A script may `require` only some Sieve extensions: see the
[list of supported extensions](../../reference/sieve-extensions/).
`tamis rules check` warns about any other extension a script requires, since
tamis would ignore what it does.
