use std::io::{BufReader, IsTerminal};
use std::process::ExitCode;
use std::sync::Arc;

use tamis::cli::{self, Env, PromptError};
use tamis::secret::{Keyring, System};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> ExitCode {
    tamis::tls::init();
    let args: Vec<String> = std::env::args().skip(1).collect();

    let shutdown = CancellationToken::new();
    tokio::spawn(stop_on_signal(shutdown.clone()));

    let stdin_is_terminal = std::io::stdin().is_terminal();
    let env = Env {
        stdin: Box::new(BufReader::new(std::io::stdin())),
        stdout: Box::new(std::io::stdout()),
        stderr: Box::new(std::io::stderr()),
        getenv: Arc::new(|key| std::env::var(key).ok()),
        stdout_is_terminal: std::io::stdout().is_terminal(),
        prompt: Some(Box::new(move |prompt| {
            if stdin_is_terminal {
                rpassword::prompt_password(prompt)
                    .map_err(|err| PromptError::Other(err.to_string()))
            } else {
                Err(PromptError::NotTerminal)
            }
        })),
        keyring: System::new()
            .ok()
            .map(|system| Arc::new(system) as Arc<dyn Keyring>),
        plaintext_imap: false,
        logging: Some(Box::new(|verbose, journal| {
            cli::logging::init(verbose, journal.as_deref());
        })),
        version: option_env!("TAMIS_VERSION")
            .unwrap_or(env!("CARGO_PKG_VERSION"))
            .to_owned(),
    };

    let code = cli::run(&args, env, shutdown).await;
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// Cancels `shutdown` on SIGINT or SIGTERM.
async fn stop_on_signal(shutdown: CancellationToken) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut int)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) else {
            return;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    shutdown.cancel();
}
