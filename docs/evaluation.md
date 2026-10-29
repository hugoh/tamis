# Evaluating the filter

`cmd/tamis-eval` measures how a change to the tokenizer or the features
affects the filter, on mail you label. It is a development tool and is not
part of the `tamis` binary.

## How it measures

[`internal/eval`](../internal/eval) does progressive validation: each message
is scored by a model that has learnt only from the messages before it, then
learnt from. That is how tamis meets mail, and it needs no train/test split.

- The model, tokenizer and scoring are the production ones (`internal/classify`),
  so an experiment can't drift from what tamis does. The first variant,
  `baseline`, is exactly that, including the recipient status token.
- The first `--warmup` messages (default 200) are learnt but not scored.
- Run 0 keeps the order given. Runs 1 to `--runs`-1 shuffle the messages, the
  same way for every variant, so the runs are paired. The report shows each
  variant's change from the first, run by run, and in how many runs it was
  better: a difference that doesn't survive that is noise.
- `(1-AUC)%` is the chance that a random spam message scores below a random
  good one. The other columns are rates at tamis's default thresholds
  (`spam_threshold` 0.9, `good_threshold` 0.2) on run 0.

## Getting data

`tamis-eval run DIR` reads `DIR/ham` and `DIR/spam`; any files below them are
messages. Each class is ordered by its `Date` headers, and the spam is spread
evenly through the good mail's time line, since a junk folder is purged and
its dates don't overlap an old archive's.

**From your mailbox:**

```sh
tamis-eval fetch --spam Junk=1000 --ham Archive=2500 ~/tmp/eval-data
tamis-eval run ~/tmp/eval-data
rm -r ~/tmp/eval-data
```

`fetch` uses the tamis configuration and keychain entry, peeks at messages
(nothing is marked as read) and writes them with mode 0600. It refuses to
write inside a git or jj working tree, because jj snapshots every file it
finds. Keep the copy outside the repository and delete it when you are done:
it is your mail.

**From the public Enron-Spam corpus** (Metsis, Androutsopoulos and Paliouras,
CEAS 2006), whose raw form is at
`https://www2.aueb.gr/users/ion/data/enron-spam/`: extract a user's ham
archive into `DIR/ham` and a spam archive into `DIR/spam`. Beware: its ham and
spam come from different sources and eras, so features that describe where a
message came from (content type, charset, whether it is HTML) look far better
than they would on real mail.

## Trying a feature

A feature is a `Featurizer`: `Tokens(raw, model)` returns the tokens to score
(it may consult the model, as the recipient status does), and
`Learn(raw, spam)` is called after the message was scored, so it can remember
what it has seen. Add a `Variant` to `eval.Standard` and run. When the feature
is adopted into the tokenizer, it becomes the baseline and is removed from
`Standard`; an ablation (`- recipient tokens`) can stay, to show what a
feature contributes.

## Results so far

On one mailbox (423 Junk messages against 2,500 Archive messages), against the
tokenizer as it was then:

- Adjacent word pairs, skip-gram pairs, `List-Id`, content type, HTML tags and
  Reply-To: no gain, or worse.
- Recipients: `rcpt:mine/other/none` (whether a To or Cc address is one that
  good mail has recently been sent to) and a `to:<address>` token per
  recipient improved the ranking in every run, by about 15% together. 36% of
  the spam went to an address not seen in recent good mail, against 0.7% of
  the good mail. These are now in the tokenizer (see Recipients in
  [`engine.md`](engine.md)), and the `- recipient tokens` variant shows what
  they contribute.

One mailbox is one data point: run your own.
