use std::io::Write as _;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    tamis::tls::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::io::stdout();
    match tamis::eval::cli::run(&args, &mut out, &|key| std::env::var(key).ok()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "tamis-eval: {err:#}");
            ExitCode::FAILURE
        }
    }
}
