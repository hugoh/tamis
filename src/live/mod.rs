//! What the running daemon knows about itself right now, for the web
//! interface to show: what the last pass did, what failed, and the recent log
//! lines.

mod status;

pub use status::{SETTING_KEY, Snapshot, Status, Waiting};
