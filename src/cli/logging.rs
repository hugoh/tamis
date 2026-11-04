//! Logging to stderr, including debug lines under `--verbose`. Under systemd
//! (which sets `$JOURNAL_STREAM` for a service whose output goes to the
//! journal) each line carries its priority, for `journalctl -p`, and no
//! timestamp, since the journal adds its own.

use std::fmt;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tracing::{Event, Level, Subscriber};

use crate::live::{DebugSwitch, Ring, RingLayer};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{self, FormatEvent, FormatFields, Writer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{EnvFilter, Layer, Registry, reload};

/// The syslog priority marker the journal reads (sd-daemon(3)) for a level:
/// err, warning, info, debug.
#[must_use]
pub fn journal_marker(level: Level) -> &'static str {
    match level {
        Level::ERROR => "<3>",
        Level::WARN => "<4>",
        Level::INFO => "<6>",
        _ => "<7>",
    }
}

/// Reports whether stderr is the journal's stream. systemd names it in
/// `$JOURNAL_STREAM` as `device:inode`, and a service that redirected its
/// stderr elsewhere still inherits the variable. Where stderr can't be
/// checked, the variable decides.
#[must_use]
pub fn under_journal(stream: Option<&str>) -> bool {
    let Some(stream) = stream.filter(|s| !s.is_empty()) else {
        return false;
    };
    stderr_stream().is_none_or(|own| own == stream)
}

#[cfg(unix)]
fn stderr_stream() -> Option<String> {
    use std::os::fd::AsFd;
    use std::os::unix::fs::MetadataExt;

    let stderr = std::io::stderr().as_fd().try_clone_to_owned().ok()?;
    let metadata = std::fs::File::from(stderr).metadata().ok()?;
    Some(format!("{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn stderr_stream() -> Option<String> {
    None
}

/// Prefixes every line with its journal priority.
struct Journal(format::Format<format::Compact, ()>);

impl<S, N> FormatEvent<S, N> for Journal
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        write!(writer, "{}", journal_marker(*event.metadata().level()))?;
        self.0.format_event(ctx, writer, event)
    }
}

/// The filter: tamis at `info`, or `debug` under `--verbose`, and everything
/// else at `warn`, so the libraries' own chatter stays out of the way. The
/// IMAP parser crates are held to `error`: they warn about every server
/// response with an empty text, as iCloud sends, and say nothing we can act on.
/// `$TAMIS_LOG` replaces it, in the `EnvFilter` syntax.
fn filter(verbose: bool) -> EnvFilter {
    EnvFilter::try_from_env("TAMIS_LOG").unwrap_or_else(|_| {
        EnvFilter::new(if verbose {
            "warn,tamis=debug"
        } else {
            "warn,tamis=info"
        })
    })
}

/// What the web interface shows and controls of the log.
pub struct Live {
    pub ring: Arc<Ring>,
    pub debug: Arc<DebugSwitch>,
}

static LIVE: OnceLock<Live> = OnceLock::new();

/// The log's ring and debug switch, once [`init`] has installed the logger.
#[must_use]
pub fn live() -> Option<&'static Live> {
    LIVE.get()
}

const RING_SIZE: usize = 1000;
const DEBUG_SPAN: Duration = Duration::from_mins(15);

/// Installs the global logger. A second call does nothing.
pub fn init(verbose: bool, journal_stream: Option<&str>) {
    use std::io::IsTerminal;

    let fmt_layer: Box<dyn Layer<Registry> + Send + Sync> = if under_journal(journal_stream) {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_target(false)
            .with_ansi(false)
            .event_format(Journal(
                format::format()
                    .compact()
                    .without_time()
                    .with_target(false)
                    .with_ansi(false),
            ))
            .boxed()
    } else {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_target(false)
            .with_ansi(std::io::stderr().is_terminal())
            .boxed()
    };

    let ring = Arc::new(Ring::new(RING_SIZE));
    let (filter_layer, handle) = reload::Layer::new(filter(verbose));
    let installed = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(RingLayer(Arc::clone(&ring)))
        .with(filter_layer)
        .try_init();
    if installed.is_err() {
        return;
    }

    let debug = DebugSwitch::new(verbose, DEBUG_SPAN, Arc::clone(&ring), move |on| {
        let next = if on {
            EnvFilter::new("warn,tamis=debug")
        } else {
            filter(verbose)
        };
        let _ = handle.reload(next);
    });
    let _ = LIVE.set(Live {
        ring,
        debug: Arc::new(debug),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_map_to_syslog_priorities() {
        let cases = [
            (Level::ERROR, "<3>"),
            (Level::WARN, "<4>"),
            (Level::INFO, "<6>"),
            (Level::DEBUG, "<7>"),
            (Level::TRACE, "<7>"),
        ];
        for (level, want) in cases {
            assert_eq!(journal_marker(level), want);
        }
    }

    #[test]
    fn without_a_journal_stream_variable_there_is_no_journal() {
        assert!(!under_journal(None));
        assert!(!under_journal(Some("")));
    }

    #[test]
    fn a_stream_naming_another_file_is_not_ours() {
        // stderr under test is a pipe or a file; 0:0 is neither.
        assert!(!under_journal(Some("0:0")) || stderr_stream().is_none());
    }

    #[test]
    fn libraries_stay_quiet_whatever_the_verbosity() {
        for (verbose, ours) in [(false, "tamis=info"), (true, "tamis=debug")] {
            let text = filter(verbose).to_string();
            assert!(text.contains(ours) && text.contains("warn"), "{text}");
        }
    }
}
