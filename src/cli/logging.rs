//! Logging to stderr, including debug lines under `--verbose`. Under systemd
//! (which sets `$JOURNAL_STREAM` for a service whose output goes to the
//! journal) each line carries its priority, for `journalctl -p`, and no
//! timestamp, since the journal adds its own.

use std::fmt;

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{self, FormatEvent, FormatFields, Writer};
use tracing_subscriber::registry::LookupSpan;

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

/// Installs the global logger. A second call does nothing.
pub fn init(verbose: bool, journal_stream: Option<&str>) {
    let level = if verbose { Level::DEBUG } else { Level::INFO };
    let builder = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .with_target(false);

    let installed = if under_journal(journal_stream) {
        builder
            .with_ansi(false)
            .event_format(Journal(
                format::format()
                    .compact()
                    .without_time()
                    .with_target(false)
                    .with_ansi(false),
            ))
            .try_init()
    } else {
        builder.try_init()
    };
    let _ = installed;
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
}
