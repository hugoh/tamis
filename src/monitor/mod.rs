//! Reports to a dead-man's-switch service such as healthchecks.io: it pings
//! when things work, says when they fail, and the service alerts when the
//! pings stop. It never holds up the daemon.

mod ping;
mod quality;
mod settings;

pub use ping::{Error, Pinger};
pub use quality::take_quality_slot;
pub use settings::{Alert, Check, Inputs, Settings, recommend};
