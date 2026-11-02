# Monitoring

`tamis run` can report to a dead-man's-switch service such as
[healthchecks.io](https://healthchecks.io), which alerts you when the pings stop
or say something failed. Nothing is sent unless `[monitor]` is configured, and a
slow or unreachable service never holds up a pass.

```toml
[monitor]
ping_url = "https://hc-ping.com/your-heartbeat-uuid"
quality_url = "https://hc-ping.com/your-quality-uuid"
fail_after = 3
```

The URLs are secrets: anyone who has one can fake a ping. Tamis refuses a config
file other users can write, never logs a URL (only its host) and never puts one in
an error message. Any service that takes `POST`s on a URL and a `/fail` suffix
works, including a self-hosted Healthchecks.

## What is sent

Two independent checks, both optional.

**Heartbeat** (`ping_url`). It says whether the daemon is judging mail:

| Event | Ping |
|---|---|
| A pass succeeds | Success, with the pass summary (counts only) as its text; at most once a minute, so a burst of mail doesn't ping every second. |
| A pass fails, fewer than `fail_after` times in a row | Nothing. The check's grace time absorbs a short outage. |
| The `fail_after`th and each later failure in a row | `/fail`, with how many passes failed, since when, and the error. The retries are paced by the back-off, so this is not a flood. |
| The login is rejected | `/fail` at once; only you can fix it. |
| Back to normal | The next success ping, sent at once, which the service reads as a recovery. |
| The daemon stops, hangs or its host goes down | Nothing: the check goes down after its period plus grace. Stopping `tamis run` is silent on purpose, because a stopped daemon is something to alert on. |

**Quality** (`quality_url`). Once a day (and right after the first pass), the
checks `tamis doctor` makes of the filter's training and decisions: class
balance, uncertain rate, correction rate, a recent decision, the journal. All ok
is a success ping; otherwise `/fail`, listing what looks wrong. The setup
(configuration, rules, web interface) and the daemon itself are not part of it;
the heartbeat covers the daemon.

IDLE connections and individual decisions send nothing: being connected says
nothing about whether mail is being judged, and a ping per decision would be noise.
No sender or subject is ever sent. Texts hold counts, times and error messages.

If a ping can't be delivered (the network, a 5xx, a 429), it is tried up to three
times in a few seconds; a 4xx, such as a wrong URL, is not retried. Trouble
delivering is logged like other trouble: at first, now and then while it lasts, and
when it ends.

## Setting the checks up

`tamis monitor` works the settings out from `policy.poll_interval` and
`monitor.fail_after`, so they don't have to be guessed:

```text
$ tamis monitor
Create two checks on the service:
  CHECK            PERIOD  GRACE  CONFIG KEY
  tamis heartbeat  2m      11m    monitor.ping_url
  tamis quality    1d      2h     monitor.quality_url

They would alert after:
  13m      the daemon stops, hangs or its host goes down
  6m       the mail server is unreachable (fail_after = 3)
  at once  the login is rejected
  1d       the filter's training or decisions look unhealthy
```

- **Heartbeat period** is `poll_interval` (at least a minute, the service's
  minimum): a healthy daemon passes at least that often, even with no mail.
- **Heartbeat grace** is the longest a pass may take (ten minutes) plus a
  minute, the same allowance `tamis doctor` makes before calling the daemon quiet.
- **The mail server being unreachable** alerts at the `fail_after`th failed pass.
  Passes are retried after `poll_interval`, doubling up to fifteen minutes, so
  with the defaults it is about two plus four minutes after the first failure.
- **Quality** is pinged daily; the grace of two hours covers a restart.

`tamis monitor --json` prints the same with the healthchecks.io API's field names
(`timeout`, `grace`, in seconds), and the text output ends with `curl` commands
that create both checks through the API. After changing `poll_interval` or
`fail_after`, run `tamis monitor` again and update the checks.

`tamis monitor test` sends one success ping to each configured check and says
whether the service took it, so a mistyped URL is found before it matters.

The monitoring settings, like the web interface's, are read when `tamis run`
starts: restart it after editing them. A SIGHUP reloads `poll_interval` and the
rest of the policy, but not `[monitor]`.
