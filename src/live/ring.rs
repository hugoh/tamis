//! The last log lines, kept only while someone wants to read them.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::Mutex;

use jiff::Timestamp;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub at: Timestamp,
    pub level: Level,
    pub text: String,
}

/// A fixed number of the newest [`Entry`]s. It holds nothing, and costs next
/// to nothing, until [`Ring::start`].
#[derive(Debug)]
pub struct Ring {
    size: usize,
    entries: Mutex<Option<VecDeque<Entry>>>,
}

impl Ring {
    #[must_use]
    pub fn new(size: usize) -> Self {
        Self {
            size,
            entries: Mutex::new(None),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<VecDeque<Entry>>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Starts keeping entries, without losing any already kept.
    pub fn start(&self) {
        self.lock()
            .get_or_insert_with(|| VecDeque::with_capacity(self.size));
    }

    /// Stops keeping entries and forgets them.
    pub fn stop(&self) {
        *self.lock() = None;
    }

    pub fn add(&self, entry: Entry) {
        if let Some(entries) = self.lock().as_mut() {
            if entries.len() == self.size {
                entries.pop_front();
            }
            entries.push_back(entry);
        }
    }

    /// What is kept, newest first.
    #[must_use]
    pub fn entries(&self) -> Vec<Entry> {
        self.lock()
            .as_ref()
            .map(|entries| entries.iter().rev().cloned().collect())
            .unwrap_or_default()
    }
}

/// Collects an event's message and fields as `message key=value …`.
#[derive(Default)]
struct Text {
    message: String,
    fields: String,
}

impl Visit for Text {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }
}

/// The per-request web log line, which would otherwise record the log view's own polling.
pub const WEB_REQUEST: &str = "web request";

/// Feeds a [`Ring`] with every event the subscriber lets through.
pub struct RingLayer(pub std::sync::Arc<Ring>);

impl<S: Subscriber> Layer<S> for RingLayer {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let mut text = Text::default();
        event.record(&mut text);
        if text.message == WEB_REQUEST {
            return;
        }
        self.0.add(Entry {
            at: Timestamp::now(),
            level: *event.metadata().level(),
            text: text.message + &text.fields,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tracing_subscriber::prelude::*;

    use super::*;

    fn entry(text: &str) -> Entry {
        Entry {
            at: Timestamp::UNIX_EPOCH,
            level: Level::INFO,
            text: text.to_owned(),
        }
    }

    #[test]
    fn a_ring_keeps_nothing_until_started() {
        let ring = Ring::new(3);

        ring.add(entry("early"));
        ring.start();
        ring.add(entry("kept"));

        assert_eq!(ring.entries(), [entry("kept")]);
    }

    #[test]
    fn the_newest_entries_come_first_and_the_oldest_fall_off() {
        let ring = Ring::new(3);
        ring.start();

        for name in ["a", "b", "c", "d"] {
            ring.add(entry(name));
        }
        ring.start();

        assert_eq!(ring.entries(), [entry("d"), entry("c"), entry("b")]);
    }

    #[test]
    fn stopping_forgets_everything() {
        let ring = Ring::new(3);
        ring.start();
        ring.add(entry("a"));

        ring.stop();

        assert!(ring.entries().is_empty());
    }

    #[test]
    fn the_layer_skips_web_requests() {
        let ring = Arc::new(Ring::new(5));
        ring.start();
        let subscriber = tracing_subscriber::registry().with(RingLayer(Arc::clone(&ring)));

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!("web request");
        });

        assert!(ring.entries().is_empty());
    }

    #[test]
    fn the_layer_records_the_message_and_fields() {
        let ring = Arc::new(Ring::new(5));
        ring.start();
        let subscriber = tracing_subscriber::registry().with(RingLayer(Arc::clone(&ring)));

        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(folder = "INBOX", uid = 7, "pass failed");
        });

        let entries = ring.entries();
        assert_eq!(
            (entries[0].level, entries[0].text.as_str()),
            (Level::WARN, "pass failed folder=INBOX uid=7")
        );
    }
}
