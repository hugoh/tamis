use jiff::Timestamp;
use mail_parser::MessageParser;
use rusqlite::types::Value;
use rusqlite::{Row, params, params_from_iter};

use super::decisions::like_pattern;
use super::{Error, Result, Store, push_page, unix};

/// A whitelisted sender.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    pub address: String,
    pub source: String,
    pub added_at: Timestamp,
}

/// The columns an address query can sort by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AddressSort {
    #[default]
    Address,
    Source,
    Added,
}

impl AddressSort {
    /// The sort a name stands for; anything unknown sorts by address.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "source" => Self::Source,
            "added" => Self::Added,
            _ => Self::Address,
        }
    }

    fn column(self) -> &'static str {
        match self {
            Self::Address => "address",
            Self::Source => "source COLLATE NOCASE",
            Self::Added => "added_at",
        }
    }
}

/// Picks and orders whitelist entries. The default is every address,
/// alphabetically.
#[derive(Clone, Debug, Default)]
pub struct AddressQuery {
    /// Keeps addresses or sources that contain it, ignoring ASCII case.
    pub search: String,
    pub sort: AddressSort,
    /// Sorts downwards.
    pub desc: bool,
    pub offset: i64,
    /// The page size; it defaults to 100.
    pub limit: i64,
}

const SEARCH_CONDITION: &str =
    r"(LOWER(address) LIKE ? ESCAPE '\' OR LOWER(source) LIKE ? ESCAPE '\')";

fn where_clause(search: &str) -> (String, Vec<Value>) {
    if search.is_empty() {
        return (String::new(), Vec::new());
    }
    let pattern = Value::Text(like_pattern(search));
    (
        format!(" WHERE {SEARCH_CONDITION}"),
        vec![pattern.clone(), pattern],
    )
}

fn read_address(row: &Row<'_>) -> rusqlite::Result<Address> {
    Ok(Address {
        address: row.get(0)?,
        source: row.get(1)?,
        added_at: unix(row.get(2)?),
    })
}

/// Extracts the lowercased address from an email address, with or without a
/// display name.
///
/// # Errors
/// [`Error::InvalidAddress`] when `input` is not exactly one address.
pub fn parse_address(input: &str) -> Result<String> {
    let invalid = || Error::InvalidAddress(input.to_owned());

    let header = format!("To: {}\r\n\r\n", input.trim());
    let message = MessageParser::default()
        .parse(header.as_bytes())
        .ok_or_else(invalid)?;
    let Some(list) = message.to().filter(|list| !list.has_groups()) else {
        return Err(invalid());
    };

    let mut mailboxes = list.mailboxes();
    match (mailboxes.next(), mailboxes.next()) {
        (Some(one), None) => one
            .address()
            .filter(|address| {
                address
                    .split_once('@')
                    .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty())
            })
            .filter(|address| !address.contains(char::is_whitespace))
            .map(str::to_lowercase)
            .ok_or_else(invalid),
        _ => Err(invalid()),
    }
}

/// The source recorded for addresses a person whitelisted by hand.
pub const MANUAL_SOURCE: &str = "manual";

/// Parses addresses the user named, returning them sorted and without
/// repeats.
///
/// # Errors
/// [`Error::NoAddresses`] when none are given, or [`Error::InvalidAddress`]
/// for the first that is not exactly one address.
pub fn parse_addresses(given: &[String]) -> Result<Vec<String>> {
    let mut addresses = given
        .iter()
        .map(|input| parse_address(input))
        .collect::<Result<Vec<_>>>()?;
    addresses.sort();
    addresses.dedup();
    if addresses.is_empty() {
        return Err(Error::NoAddresses);
    }
    Ok(addresses)
}

impl Store {
    /// Whitelists the address in `given`, as a person would by hand, and
    /// returns it as stored.
    ///
    /// # Errors
    /// [`Error::InvalidAddress`] when `given` is not exactly one address, or a
    /// database error.
    pub fn whitelist(&self, given: &str) -> Result<String> {
        let address = parse_address(given)?;
        self.add_address(&address, MANUAL_SOURCE)?;
        Ok(address)
    }

    /// Counts the entries a search matches.
    ///
    /// # Errors
    /// [`Error::NoMatch`] when there are none, or a database error.
    pub fn count_matching(&self, search: &str) -> Result<i64> {
        let (_, total) = self.query_addresses(&AddressQuery {
            search: search.to_owned(),
            limit: 1,
            ..AddressQuery::default()
        })?;
        if total == 0 {
            return Err(Error::NoMatch);
        }
        Ok(total)
    }

    /// Takes the given (already parsed) addresses off the whitelist, if all
    /// of them are on it, and returns how many were removed.
    ///
    /// # Errors
    /// [`Error::NotWhitelisted`] for the first that is not on the whitelist,
    /// removing none, or a database error.
    pub fn remove_listed(&self, addresses: &[String]) -> Result<usize> {
        for address in addresses {
            if !self.has_address(address)? {
                return Err(Error::NotWhitelisted(address.clone()));
            }
        }
        self.remove_addresses(addresses)
    }

    /// Adds an address to the whitelist. Addresses compare case-insensitively.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn add_address(&self, address: &str, source: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO addresses (address, source, added_at) VALUES (?, ?, ?)
             ON CONFLICT(address) DO NOTHING",
            params![address.to_lowercase(), source, Timestamp::now().as_second()],
        )?;
        Ok(())
    }

    /// Takes an address off the whitelist, if it is there.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn remove_address(&self, address: &str) -> Result<()> {
        self.conn().execute(
            "DELETE FROM addresses WHERE address = ?",
            [address.to_lowercase()],
        )?;
        Ok(())
    }

    /// Reports whether `address` is whitelisted.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn has_address(&self, address: &str) -> Result<bool> {
        let found: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM addresses WHERE address = ?",
            [address.to_lowercase()],
            |row| row.get(0),
        )?;
        Ok(found > 0)
    }

    /// The whitelist, sorted by address.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn addresses(&self) -> Result<Vec<Address>> {
        self.read_addresses(
            "SELECT address, source, added_at FROM addresses ORDER BY address",
            &[],
        )
    }

    /// One page of the whitelist and how many entries match in all.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn query_addresses(&self, query: &AddressQuery) -> Result<(Vec<Address>, i64)> {
        let (where_clause, mut args) = where_clause(&query.search);
        let total = self.conn().query_row(
            &format!("SELECT COUNT(*) FROM addresses{where_clause}"),
            params_from_iter(args.iter()),
            |row| row.get(0),
        )?;

        push_page(&mut args, query.limit, query.offset);

        let direction = if query.desc { "DESC" } else { "ASC" };
        let entries = self.read_addresses(
            &format!(
                "SELECT address, source, added_at FROM addresses{where_clause} ORDER BY {} {direction}, address ASC LIMIT ? OFFSET ?",
                query.sort.column()
            ),
            &args,
        )?;
        Ok((entries, total))
    }

    /// Takes the given addresses off the whitelist, in one statement, and
    /// returns how many were on it. Case does not matter.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn remove_addresses(&self, addresses: &[String]) -> Result<usize> {
        let lowered: Vec<String> = addresses.iter().map(|a| a.to_lowercase()).collect();
        Ok(self.conn().execute(
            "DELETE FROM addresses WHERE address IN (SELECT value FROM json_each(?))",
            [serde_json::to_string(&lowered)?],
        )?)
    }

    /// Removes every entry a [`Store::query_addresses`] with the same search
    /// would list, and returns how many. An empty search matches them all.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn remove_addresses_matching(&self, search: &str) -> Result<usize> {
        let (where_clause, args) = where_clause(search);
        Ok(self.conn().execute(
            &format!("DELETE FROM addresses{where_clause}"),
            params_from_iter(args.iter()),
        )?)
    }

    fn read_addresses(&self, sql: &str, args: &[Value]) -> Result<Vec<Address>> {
        let conn = self.conn();
        let mut statement = conn.prepare(sql)?;
        let rows = statement.query_map(params_from_iter(args.iter()), read_address)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}
