mod common;
mod init;
mod log;
mod report;
mod rules;
mod session;
#[cfg(test)]
mod tests;
mod whitelist;

use std::io::Write;

use anyhow::Result;

use super::App;

pub(super) use init::init;
pub(super) use log::log;
pub(super) use report::{doctor, monitor, monitor_test, status};
pub(super) use rules::{check as rules_check, show as rules_show, test as rules_test};
pub(super) use session::{login, logout};
pub(super) use whitelist::{
    add as whitelist_add, list as whitelist_list, remove as whitelist_remove,
};

pub(super) fn version(app: &mut App) -> Result<()> {
    writeln!(app.env.stdout, "tamis {}", app.env.version)?;
    Ok(())
}
