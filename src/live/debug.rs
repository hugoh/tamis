//! A switch that raises logging to debug for a while.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::Timestamp;
use tokio::task::AbortHandle;

use super::Ring;

type Apply = Arc<dyn Fn(bool) + Send + Sync>;

/// Turns debug logging on for a span of time and back off, keeping the
/// [`Ring`] only while it is on. When the daemon already runs at debug
/// (`--verbose`) the switch is locked on and does nothing.
pub struct DebugSwitch {
    locked: bool,
    span: Duration,
    apply: Apply,
    ring: Arc<Ring>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    until: Option<Timestamp>,
    timer: Option<AbortHandle>,
}

impl DebugSwitch {
    /// `apply(true)` must raise the log level to debug and `apply(false)`
    /// restore it.
    #[must_use]
    pub fn new(
        locked: bool,
        span: Duration,
        ring: Arc<Ring>,
        apply: impl Fn(bool) + Send + Sync + 'static,
    ) -> Self {
        if locked {
            ring.start();
        }
        Self {
            locked,
            span,
            apply: Arc::new(apply),
            ring,
            state: Mutex::new(State::default()),
        }
    }

    #[must_use]
    pub fn locked(&self) -> bool {
        self.locked
    }

    #[must_use]
    pub fn span(&self) -> Duration {
        self.span
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether it is on, and until when.
    #[must_use]
    pub fn status(&self) -> (bool, Option<Timestamp>) {
        let until = self.state().until;
        (until.is_some(), until)
    }

    /// Turns debug on for the span, or restarts the span when already on.
    pub fn on(self: &Arc<Self>) {
        if self.locked {
            return;
        }
        let mut state = self.state();
        if let Some(timer) = state.timer.take() {
            timer.abort();
        }
        self.ring.start();
        (self.apply)(true);
        state.until = Timestamp::now().checked_add(self.span).ok();
        let this = Arc::clone(self);
        let span = self.span;
        state.timer = Some(
            tokio::spawn(async move {
                tokio::time::sleep(span).await;
                this.off();
            })
            .abort_handle(),
        );
    }

    pub fn off(&self) {
        if self.locked {
            return;
        }
        let mut state = self.state();
        if let Some(timer) = state.timer.take() {
            timer.abort();
        }
        (self.apply)(false);
        self.ring.stop();
        state.until = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    fn switch(locked: bool, span: Duration) -> (Arc<DebugSwitch>, Arc<AtomicBool>, Arc<Ring>) {
        let level = Arc::new(AtomicBool::new(false));
        let ring = Arc::new(Ring::new(10));
        let seen = Arc::clone(&level);
        (
            Arc::new(DebugSwitch::new(
                locked,
                span,
                Arc::clone(&ring),
                move |on| seen.store(on, Ordering::SeqCst),
            )),
            level,
            ring,
        )
    }

    fn kept(ring: &Ring) -> bool {
        ring.add(super::super::Entry {
            at: Timestamp::UNIX_EPOCH,
            level: tracing::Level::INFO,
            text: String::new(),
        });
        !ring.entries().is_empty()
    }

    #[tokio::test]
    async fn on_raises_the_level_and_keeps_log_lines_until_off() {
        let (debug, level, ring) = switch(false, Duration::from_mins(15));

        debug.on();
        let (on, until) = debug.status();
        let while_on = (level.load(Ordering::SeqCst), kept(&ring));
        debug.off();

        assert!(on && until.is_some());
        assert_eq!(while_on, (true, true));
        assert_eq!(
            (
                level.load(Ordering::SeqCst),
                debug.status().0,
                ring.entries().len()
            ),
            (false, false, 0)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn it_turns_itself_off_after_the_span() {
        let (debug, level, _) = switch(false, Duration::from_mins(15));
        debug.on();

        tokio::time::sleep(Duration::from_mins(16)).await;

        assert_eq!(
            (level.load(Ordering::SeqCst), debug.status().0),
            (false, false)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn turning_it_on_again_restarts_the_span() {
        let (debug, level, _) = switch(false, Duration::from_mins(15));
        debug.on();
        tokio::time::sleep(Duration::from_mins(10)).await;

        debug.on();
        tokio::time::sleep(Duration::from_mins(10)).await;

        assert!(
            level.load(Ordering::SeqCst),
            "the first timer must not fire"
        );
    }

    #[tokio::test]
    async fn a_locked_switch_keeps_logs_and_ignores_the_buttons() {
        let (debug, level, ring) = switch(true, Duration::from_mins(15));

        debug.on();
        debug.off();

        assert!(debug.locked() && kept(&ring));
        assert_eq!(
            (level.load(Ordering::SeqCst), debug.status().0),
            (false, false)
        );
    }
}
