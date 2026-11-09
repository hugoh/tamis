//! What the running daemon knows about itself right now, for the web
//! interface to show: what the last pass did, what failed, and the recent log
//! lines.

mod debug;
mod ring;
mod status;

pub use debug::DebugSwitch;
pub use ring::{Entry, Ring, RingLayer, WEB_REQUEST};
pub use status::{SETTING_KEY, Snapshot, Status};
