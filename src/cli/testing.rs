//! Helpers for the CLI's tests.

use std::io::Write;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use super::{Env, run};
use crate::secret::{Keyring, Memory};

/// Output collected for a test to read.
#[derive(Clone, Default)]
pub(crate) struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(&mut *self.0.lock().unwrap(), buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// An environment with the given variables and standard input, a keychain in
/// memory, and no terminal.
pub(crate) struct Harness {
    pub env: Env,
    pub out: Capture,
    pub err: Capture,
    pub keyring: Arc<Memory>,
}

impl Harness {
    pub fn new(vars: &[(&str, &str)], stdin: &str) -> Self {
        let (out, err) = (Capture::default(), Capture::default());
        let keyring = Arc::new(Memory::default());
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let env = Env {
            stdin: Box::new(std::io::Cursor::new(stdin.as_bytes().to_vec())),
            stdout: Box::new(out.clone()),
            stderr: Box::new(err.clone()),
            getenv: Arc::new(move |key| {
                vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
            }),
            stdout_is_terminal: false,
            prompt: None,
            keyring: Some(Arc::clone(&keyring) as Arc<dyn Keyring>),
            plaintext_imap: true,
            version: "1.2.3".to_owned(),
            logging: None,
        };
        Self {
            env,
            out,
            err,
            keyring,
        }
    }

    pub fn without_keyring(mut self) -> Self {
        self.env.keyring = None;
        self
    }

    /// Runs the command line and returns its exit code.
    pub async fn run(self, args: &[&str]) -> (i32, Capture, Capture) {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let code = run(&args, self.env, CancellationToken::new()).await;
        (code, self.out, self.err)
    }
}

/// An IMAP server that accepts or rejects every login; returns its port.
pub(crate) async fn imap_server(accept: bool) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                let _ = write
                    .write_all(b"* OK [CAPABILITY IMAP4rev1 IDLE UIDPLUS MOVE] ready\r\n")
                    .await;
                while let Ok(Some(line)) = lines.next_line().await {
                    let (tag, command) = line.split_once(' ').unwrap_or((&line, ""));
                    let reply = if command.starts_with("CAPABILITY") {
                        format!("* CAPABILITY IMAP4rev1 IDLE UIDPLUS MOVE\r\n{tag} OK done\r\n")
                    } else if command.starts_with("LOGIN") && !accept {
                        format!("{tag} NO [AUTHENTICATIONFAILED] bad password\r\n")
                    } else if command.starts_with("LOGOUT") {
                        format!("* BYE bye\r\n{tag} OK done\r\n")
                    } else {
                        format!("{tag} OK done\r\n")
                    };
                    let _ = write.write_all(reply.as_bytes()).await;
                }
            });
        }
    });
    port
}

/// An HTTP server that answers every request with `status`; returns its URL.
pub(crate) async fn http_server(status: u16) -> String {
    use tokio::io::AsyncReadExt;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/check", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0_u8; 4096];
                let _ = stream.read(&mut buffer).await;
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    url
}
