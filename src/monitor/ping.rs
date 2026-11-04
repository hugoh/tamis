use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use reqwest::StatusCode;
use thiserror::Error;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::retry::{self, Backoff, Streak, log_at};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const ATTEMPTS: usize = 3;
/// Keeps a ping's text well inside what services accept.
const MAX_BODY: usize = 10 << 10;
/// Caps the wait between tries of one ping.
const RESEND_WAIT: Duration = Duration::from_secs(5);
/// Spaces the success pings: a burst of mail makes a burst of passes.
const MIN_GAP: Duration = Duration::from_mins(1);

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("monitor URL: {0}")]
    Url(String),
    #[error("unexpected response: HTTP {0}")]
    Status(u16),
    #[error("ping: {0}")]
    Network(String),
}

impl Error {
    /// A network failure, or a service that is overloaded or broken.
    fn retryable(&self) -> bool {
        match self {
            Self::Status(code) => *code == 429 || *code >= 500,
            Self::Network(_) => true,
            Self::Url(_) => false,
        }
    }
}

struct Ping {
    path: &'static str,
    body: String,
}

#[derive(Default)]
struct State {
    /// Only the newest ping that hasn't gone out is kept: it is the truth now.
    pending: Option<Ping>,
    last_ok: Option<Instant>,
}

struct Shared {
    base: Url,
    client: reqwest::Client,
    state: Mutex<State>,
    wake: Notify,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn queue(&self, mut state: MutexGuard<'_, State>, next: Ping) {
        state.pending = Some(next);
        drop(state);
        self.wake.notify_one();
    }

    async fn deliver(&self, path: &str, body: &str) -> Result<(), Error> {
        let mut target = self.base.clone();
        if !path.is_empty() {
            let joined = format!("{}/{path}", target.path().trim_end_matches('/'));
            target.set_path(&joined);
        }
        let body = truncate(body, MAX_BODY);

        let response = self
            .client
            .post(target)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(body.to_owned())
            // The error holds the URL, which holds the check's secret.
            .send()
            .await
            .map_err(|err| Error::Network(err.without_url().to_string()))?;

        let status = response.status();
        if status < StatusCode::OK || status >= StatusCode::MULTIPLE_CHOICES {
            return Err(Error::Status(status.as_u16()));
        }
        Ok(())
    }

    /// Delivers a ping, trying again briefly when the service or the network
    /// hiccups.
    async fn send(&self, cancel: &CancellationToken, next: &Ping) -> Result<(), Error> {
        let mut pace = Backoff::new(Duration::from_secs(1), RESEND_WAIT);
        let mut last = Error::Network("not sent".to_owned());

        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                tokio::select! {
                    () = cancel.cancelled() => return Err(last),
                    () = tokio::time::sleep(pace.next_delay()) => {}
                }
            }
            match self.deliver(next.path, &next.body).await {
                Ok(()) => return Ok(()),
                Err(err) if !err.retryable() => return Err(err),
                Err(err) => last = err,
            }
        }
        Err(last)
    }
}

fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

struct Inner {
    shared: Arc<Shared>,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

/// Sends pings in the background. One for an empty URL does nothing, so
/// callers needn't check whether monitoring is configured.
pub struct Pinger {
    inner: Option<Inner>,
}

impl Pinger {
    /// A pinger that does nothing.
    #[must_use]
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    /// Starts a pinger for the check at `raw_url`, or an inert one when it is
    /// empty. It runs until closed.
    ///
    /// # Errors
    /// [`Error::Url`] when the URL doesn't parse.
    pub fn new(raw_url: &str) -> Result<Self, Error> {
        if raw_url.is_empty() {
            return Ok(Self { inner: None });
        }

        let base = Url::parse(raw_url).map_err(|err| Error::Url(err.to_string()))?;
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| Error::Url(err.without_url().to_string()))?;
        let shared = Arc::new(Shared {
            base,
            client,
            state: Mutex::new(State::default()),
            wake: Notify::new(),
        });
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(Arc::clone(&shared), cancel.clone()));
        Ok(Self {
            inner: Some(Inner {
                shared,
                cancel,
                task,
            }),
        })
    }

    /// Whether monitoring is configured.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// Says the daemon works. Pings come at most once a minute.
    pub fn success(&self, body: &str) {
        let Some(inner) = &self.inner else { return };
        let mut state = inner.shared.state();
        if state.last_ok.is_some_and(|at| at.elapsed() < MIN_GAP) {
            return;
        }
        state.last_ok = Some(Instant::now());
        inner.shared.queue(
            state,
            Ping {
                path: "",
                body: body.to_owned(),
            },
        );
    }

    /// Says something is wrong. The next success is sent at once, so that the
    /// service sees the recovery.
    pub fn fail(&self, body: &str) {
        let Some(inner) = &self.inner else { return };
        let mut state = inner.shared.state();
        state.last_ok = None;
        inner.shared.queue(
            state,
            Ping {
                path: "fail",
                body: body.to_owned(),
            },
        );
    }

    /// Sends one success ping now and says whether the service took it.
    ///
    /// # Errors
    /// Why the service didn't.
    pub async fn test(&self, body: &str) -> Result<(), Error> {
        match &self.inner {
            Some(inner) => inner.shared.deliver("", body).await,
            None => Ok(()),
        }
    }

    /// Stops sending. A ping that hasn't gone out is dropped: stopping the
    /// daemon is something the service should alert on.
    pub async fn close(self) {
        if let Some(inner) = self.inner {
            inner.cancel.cancel();
            let _ = inner.task.await;
        }
    }
}

async fn run(shared: Arc<Shared>, cancel: CancellationToken) {
    let mut streak = Streak::default();
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            () = shared.wake.notified() => {}
        }

        let next = shared.state().pending.take();
        if let Some(next) = next {
            let outcome = shared.send(&cancel, &next).await;
            report(&shared, &cancel, &mut streak, outcome.err());
        }
    }
}

/// Logs delivery trouble the way other trouble is logged: when it starts, now
/// and then while it lasts, and when it ends.
fn report(shared: &Shared, cancel: &CancellationToken, streak: &mut Streak, err: Option<Error>) {
    let Some(err) = err else {
        if let Some((failures, downtime)) = streak.recover(SystemTime::now()) {
            tracing::info!(failures, ?downtime, "monitoring ping delivered again");
        }
        return;
    };
    if cancel.is_cancelled() {
        return;
    }
    let failures = streak.fail(SystemTime::now());
    let host = shared.base.host_str().unwrap_or_default();
    log_at!(retry::level(failures, tracing::Level::WARN), host, %err, failures, "monitoring ping failed");
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// What one request to the test server carried.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Request {
        path: String,
        body: String,
    }

    /// An HTTP server that answers with the given statuses in turn (200 after
    /// they run out) and records what it was sent.
    struct Server {
        url: String,
        seen: Arc<Mutex<Vec<Request>>>,
    }

    impl Server {
        async fn start(statuses: &[u16]) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let seen = Arc::new(Mutex::new(Vec::new()));
            let statuses = Arc::new(Mutex::new(
                statuses
                    .iter()
                    .copied()
                    .collect::<std::collections::VecDeque<_>>(),
            ));

            let recorded = Arc::clone(&seen);
            tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        return;
                    };
                    let (recorded, statuses) = (Arc::clone(&recorded), Arc::clone(&statuses));
                    tokio::spawn(async move {
                        let mut buffer = Vec::new();
                        let mut chunk = [0_u8; 4096];
                        let (head_end, length) = loop {
                            let n = stream.read(&mut chunk).await.unwrap_or(0);
                            if n == 0 {
                                return;
                            }
                            buffer.extend_from_slice(&chunk[..n]);
                            let text = String::from_utf8_lossy(&buffer).into_owned();
                            if let Some(end) = text.find("\r\n\r\n") {
                                let length = text
                                    .lines()
                                    .find_map(|l| {
                                        l.to_lowercase()
                                            .strip_prefix("content-length:")
                                            .map(|v| v.trim().parse().unwrap_or(0))
                                    })
                                    .unwrap_or(0_usize);
                                break (end + 4, length);
                            }
                        };
                        while buffer.len() < head_end + length {
                            let n = stream.read(&mut chunk).await.unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            buffer.extend_from_slice(&chunk[..n]);
                        }

                        let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
                        let path = head
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or_default()
                            .to_owned();
                        let body = String::from_utf8_lossy(&buffer[head_end..]).into_owned();
                        recorded.lock().unwrap().push(Request { path, body });

                        let status = statuses.lock().unwrap().pop_front().unwrap_or(200);
                        let reply = format!(
                            "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                        let _ = stream.write_all(reply.as_bytes()).await;
                    });
                }
            });
            Self { url, seen }
        }

        fn requests(&self) -> Vec<Request> {
            self.seen.lock().unwrap().clone()
        }

        /// Waits until `count` requests have arrived.
        async fn wait_for(&self, count: usize) {
            for _ in 0..200 {
                if self.requests().len() >= count {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            panic!("expected {count} requests, got {:?}", self.requests());
        }
    }

    #[tokio::test]
    async fn pings_go_to_the_check_and_failures_to_its_fail_path() {
        let server = Server::start(&[]).await;
        let pinger = Pinger::new(&format!("{}/abc/", server.url)).unwrap();

        pinger.success("all well");
        server.wait_for(1).await;
        pinger.fail("it broke");
        server.wait_for(2).await;

        assert_eq!(
            server.requests(),
            [
                Request {
                    path: "/abc/".into(),
                    body: "all well".into()
                },
                Request {
                    path: "/abc/fail".into(),
                    body: "it broke".into()
                },
            ]
        );
        pinger.close().await;
    }

    #[tokio::test]
    async fn success_pings_are_spaced_but_one_after_a_failure_goes_at_once() {
        let server = Server::start(&[]).await;
        let pinger = Pinger::new(&server.url).unwrap();

        pinger.success("one");
        pinger.success("two");
        server.wait_for(1).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(server.requests().len(), 1, "the second came too soon");

        pinger.fail("down");
        server.wait_for(2).await;
        pinger.success("recovered");
        server.wait_for(3).await;

        let paths: Vec<String> = server.requests().into_iter().map(|r| r.path).collect();
        assert_eq!(paths, ["/", "/fail", "/"]);
        pinger.close().await;
    }

    #[tokio::test]
    async fn a_service_that_hiccups_is_tried_again() {
        let server = Server::start(&[500, 503]).await;
        let pinger = Pinger::new(&server.url).unwrap();

        pinger.fail("x");
        server.wait_for(3).await;

        assert_eq!(
            server.requests().len(),
            3,
            "two failures and then the ping got through"
        );
        pinger.close().await;
    }

    #[tokio::test]
    async fn a_refused_ping_is_not_repeated() {
        let server = Server::start(&[404]).await;
        let pinger = Pinger::new(&server.url).unwrap();

        pinger.fail("x");
        server.wait_for(1).await;
        tokio::time::sleep(Duration::from_millis(1500)).await;

        assert_eq!(server.requests().len(), 1);
        pinger.close().await;
    }

    #[tokio::test]
    async fn testing_reports_what_the_service_said() {
        let server = Server::start(&[200, 404]).await;
        let pinger = Pinger::new(&server.url).unwrap();

        assert_eq!(pinger.test("hello").await, Ok(()));
        assert_eq!(pinger.test("hello").await, Err(Error::Status(404)));
        pinger.close().await;
    }

    #[tokio::test]
    async fn long_text_is_cut_to_what_services_accept() {
        let server = Server::start(&[]).await;
        let pinger = Pinger::new(&server.url).unwrap();

        pinger.success(&"é".repeat(20_000));
        server.wait_for(1).await;

        assert_eq!(
            server.requests()[0].body.len(),
            MAX_BODY,
            "cut on a character boundary"
        );
        pinger.close().await;
    }

    #[tokio::test]
    async fn network_errors_do_not_leak_the_secret_url() {
        let pinger = Pinger::new("http://127.0.0.1:1/secret-check-id").unwrap();

        let err = pinger.test("x").await.unwrap_err().to_string();

        assert!(!err.contains("secret-check-id"), "{err}");
        pinger.close().await;
    }

    #[tokio::test]
    async fn no_url_means_no_monitoring() {
        let pinger = Pinger::new("").unwrap();

        pinger.success("x");
        pinger.fail("x");
        assert!(!pinger.is_enabled());
        assert_eq!(pinger.test("x").await, Ok(()));
        pinger.close().await;

        assert!(matches!(Pinger::new("not a url"), Err(Error::Url(_))));
    }
}
