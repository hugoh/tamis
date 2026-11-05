use std::io::Write;

use anyhow::{Result, bail};

use super::super::App;
use super::common::{local, open, palette};
use crate::output::style::{BOLD, Cell, DIM, cell, write_table};
use crate::store::{Address, AddressQuery, Store, parse_address};

const MANUAL_SOURCE: &str = "manual";

pub(in super::super) fn list(app: &mut App) -> Result<()> {
    let (_, store) = open(app)?;
    let addresses = store.addresses()?;
    write_addresses(app, &addresses)
}

fn write_addresses(app: &mut App, addresses: &[Address]) -> Result<()> {
    if addresses.is_empty() {
        writeln!(app.env.stdout, "No addresses whitelisted.")?;
        return Ok(());
    }
    let colors = palette(app)?;
    let mut rows: Vec<Vec<Cell>> = vec![vec![
        cell("ADDRESS", BOLD),
        cell("SOURCE", BOLD),
        cell("ADDED", BOLD),
    ]];
    for address in addresses {
        rows.push(vec![
            cell(&address.address, ""),
            cell(&address.source, DIM),
            cell(&local(address.added_at), DIM),
        ]);
    }
    write_table(&mut app.env.stdout, colors, &rows)
}

pub(in super::super) fn add(app: &mut App, addresses: &[String]) -> Result<()> {
    let (_, store) = open(app)?;
    for given in addresses {
        let address = parse_address(given)?;
        store.add_address(&address, MANUAL_SOURCE)?;
        writeln!(app.env.stdout, "whitelisted {address}")?;
    }
    Ok(())
}

pub(in super::super) fn remove(
    app: &mut App,
    addresses: &[String],
    matching: Option<&str>,
    confirm: bool,
) -> Result<()> {
    let (_, store) = open(app)?;
    match matching {
        Some(search) => remove_matching(app, &store, search, confirm),
        None => remove_listed(app, &store, addresses),
    }
}

/// Removes the named addresses, once all of them have been checked.
fn remove_listed(app: &mut App, store: &Store, given: &[String]) -> Result<()> {
    let mut addresses = Vec::with_capacity(given.len());
    for arg in given {
        let address = parse_address(arg)?;
        if !store.has_address(&address)? {
            bail!("{address} is not whitelisted");
        }
        addresses.push(address);
    }
    store.remove_addresses(&addresses)?;
    for address in addresses {
        writeln!(app.env.stdout, "removed {address}")?;
    }
    Ok(())
}

/// Shows the addresses a search matches and, if `confirm` is set, removes
/// them.
fn remove_matching(app: &mut App, store: &Store, search: &str, confirm: bool) -> Result<()> {
    let query = AddressQuery {
        search: search.to_owned(),
        limit: i64::from(i32::MAX),
        ..AddressQuery::default()
    };
    let (found, total) = store.query_addresses(&query)?;
    if total == 0 {
        writeln!(app.env.stdout, "No addresses match.")?;
        return Ok(());
    }
    if !confirm {
        write_addresses(app, &found)?;
        let line = if total == 1 {
            "1 address matches".to_owned()
        } else {
            format!("{total} addresses match")
        };
        writeln!(
            app.env.stdout,
            "\n{line}; run again with --yes to remove them."
        )?;
        return Ok(());
    }
    let removed = store.remove_addresses_matching(search)?;
    writeln!(
        app.env.stdout,
        "removed {removed} {}",
        if removed == 1 { "address" } else { "addresses" }
    )?;
    Ok(())
}
