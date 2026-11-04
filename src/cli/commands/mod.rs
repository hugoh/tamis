mod init;
mod session;

use std::io::Write;

use anyhow::Result;

use super::App;

pub(super) use init::init;
pub(super) use session::{login, logout};

pub(super) fn version(app: &mut App) -> Result<()> {
    writeln!(app.env.stdout, "tamis {}", app.env.version)?;
    Ok(())
}
