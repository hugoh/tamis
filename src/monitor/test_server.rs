//! An HTTP server that records the pings it is sent, for tests.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// What one request to the test server carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) path: String,
    pub(crate) body: String,
}

/// An HTTP server that answers with the given statuses in turn (200 after
/// they run out) and records what it was sent.
pub(crate) struct Server {
    pub(crate) url: String,
    seen: Arc<Mutex<Vec<Request>>>,
}

impl Server {
    pub(crate) async fn start(statuses: &[u16]) -> Self {
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

    pub(crate) fn requests(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }

    /// Waits until `count` requests have arrived.
    pub(crate) async fn wait_for(&self, count: usize) {
        for _ in 0..200 {
            if self.requests().len() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("expected {count} requests, got {:?}", self.requests());
    }
}
