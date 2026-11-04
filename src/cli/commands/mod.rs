mod common;
mod engine;
mod init;
mod log;
mod probe;
mod report;
mod rules;
mod run;
mod session;
#[cfg(test)]
mod tests;
mod web;
mod whitelist;

use std::io::Write;

use anyhow::Result;

use super::App;

pub(super) use engine::{bootstrap, learn, undo};
pub(super) use init::init;
pub(super) use log::log;
pub(super) use probe::probe;
pub(super) use report::{doctor, monitor, monitor_test, status};
pub(super) use rules::{check as rules_check, show as rules_show, test as rules_test};
pub(super) use run::run;
pub(super) use session::{login, logout};
pub(super) use web::{enroll as web_enroll, passkeys as web_passkeys, revoke as web_revoke};
pub(super) use whitelist::{
    add as whitelist_add, list as whitelist_list, remove as whitelist_remove,
};

pub(super) fn version(app: &mut App) -> Result<()> {
    writeln!(app.env.stdout, "tamis {}", app.env.version)?;
    Ok(())
}
