//! The whitelist page: search, add, and remove one, some or all.

use std::collections::BTreeMap;

use askama::Template;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use url::form_urlencoded;

use super::listing::{Column, PAGE_SIZE, SortChoice, TableState, count_label};
use super::pages::{is_htmx, local_time, params, render};
use super::{App, fail};
use crate::store::{self, AddressQuery, AddressSort, parse_addresses};

const CONFIRM_SHOWN: usize = 200;

const SORTS: [SortChoice; 3] = [
    SortChoice {
        key: "address",
        label: "Address",
        asc_first: true,
    },
    SortChoice {
        key: "source",
        label: "Source",
        asc_first: true,
    },
    SortChoice {
        key: "added",
        label: "Added",
        asc_first: false,
    },
];

struct AddressRow {
    address: String,
    source: String,
    added: String,
}

#[derive(Default)]
struct Edit {
    error: String,
    note: String,
    reset_form: bool,
    clear_confirm: bool,
}

struct Body {
    sort: &'static str,
    dir: &'static str,
    search: String,
    rows: Vec<AddressRow>,
    count: String,
    columns: Vec<Column>,
    more_href: String,
    remove_all: String,
    error: String,
    note: String,
    reset_form: bool,
    clear_confirm: bool,
}

#[derive(Template)]
#[template(path = "whitelist.html")]
struct Page {
    title: &'static str,
    nav: &'static str,
    version: String,
    body: Body,
}

#[derive(Template)]
#[template(path = "whitelist_results.html")]
struct Results {
    body: Body,
}

#[derive(Template)]
#[template(path = "whitelist_rows.html")]
struct Rows {
    body: Body,
}

#[derive(Default)]
struct ConfirmBody {
    title: String,
    shown: Vec<String>,
    more: usize,
    selected: Vec<String>,
    all: bool,
    search: String,
    count: i64,
    error: String,
}

#[derive(Template)]
#[template(path = "whitelist_confirm.html")]
struct Confirm {
    confirm: ConfirmBody,
}

fn remove_all_label(total: i64, search: &str) -> String {
    match (total, search.is_empty()) {
        (0, _) => String::new(),
        (_, true) => format!("Remove all {total}…"),
        _ => format!("Remove {total} matching…"),
    }
}

fn build(app: &App, state: &TableState, edit: Edit) -> Result<Body, store::Error> {
    let (found, total) = app.db.query_addresses(&AddressQuery {
        search: state.search.clone(),
        sort: AddressSort::from_name(state.sort),
        desc: !state.asc,
        after: state.after.clone(),
        limit: PAGE_SIZE + 1,
    })?;
    let page_size = usize::try_from(PAGE_SIZE).unwrap_or(usize::MAX);
    let more = found.len() > page_size;
    let found = &found[..found.len().min(page_size)];
    Ok(Body {
        sort: state.sort,
        dir: state.dir(),
        search: state.search.clone(),
        count: count_label(total, "address", "addresses", !state.search.is_empty()),
        columns: state.columns(&SORTS),
        more_href: state.more_href(found.last().map_or("", |a| a.address.as_str()), more),
        remove_all: remove_all_label(total, &state.search),
        rows: found
            .iter()
            .map(|a| AddressRow {
                address: a.address.clone(),
                source: a.source.clone(),
                added: local_time(a.added_at),
            })
            .collect(),
        error: edit.error,
        note: edit.note,
        reset_form: edit.reset_form,
        clear_confirm: edit.clear_confirm,
    })
}

pub(super) async fn page(
    State(app): State<App>,
    headers: HeaderMap,
    query: Query<BTreeMap<String, String>>,
) -> Response {
    let state = TableState::parse("/whitelist", &params(query), &SORTS);
    let body = match build(&app, &state, Edit::default()) {
        Ok(body) => body,
        Err(err) => return fail("list whitelist", &err),
    };
    if !is_htmx(&headers) {
        return render(&Page {
            title: "whitelist",
            nav: "whitelist",
            version: app.options.version.clone(),
            body,
        });
    }
    if state.after.is_empty() {
        render(&Results { body })
    } else {
        render(&Rows { body })
    }
}

/// Fields of a form body, which can repeat.
fn form(body: &Bytes) -> Vec<(String, String)> {
    form_urlencoded::parse(body).into_owned().collect()
}

fn field<'a>(form: &'a [(String, String)], name: &str) -> &'a str {
    form.iter()
        .find(|(key, _)| key == name)
        .map_or("", |(_, value)| value.as_str())
}

fn results(app: &App, form: &[(String, String)], edit: Edit) -> Response {
    let values: BTreeMap<String, String> = form.iter().cloned().collect();
    let mut state = TableState::parse("/whitelist", &values, &SORTS);
    state.after.clear();
    match build(app, &state, edit) {
        Ok(body) => render(&Results { body }),
        Err(err) => fail("list whitelist", &err),
    }
}

pub(super) async fn add(State(app): State<App>, body: Bytes) -> Response {
    let form = form(&body);
    let edit = match app.db.whitelist(field(&form, "address")) {
        Ok(_) => Edit {
            reset_form: true,
            ..Edit::default()
        },
        Err(err) => Edit {
            error: err.to_string(),
            ..Edit::default()
        },
    };
    results(&app, &form, edit)
}

/// What a removal request asks for.
struct Removal {
    all: bool,
    search: String,
    addresses: Vec<String>,
    count: i64,
}

enum Refusal {
    User(String),
    Internal(store::Error),
}

impl From<store::Error> for Refusal {
    fn from(err: store::Error) -> Self {
        if err.is_user_error() {
            Self::User(err.to_string())
        } else {
            Self::Internal(err)
        }
    }
}

fn parse_removal(app: &App, form: &[(String, String)]) -> Result<Removal, Refusal> {
    if field(form, "scope") == "all" {
        let search = field(form, "q").trim().to_owned();
        let total = app.db.count_matching(&search)?;
        return Ok(Removal {
            all: true,
            search,
            addresses: Vec::new(),
            count: total,
        });
    }
    let given: Vec<String> = form
        .iter()
        .filter(|(name, _)| name == "address")
        .map(|(_, value)| value.clone())
        .collect();
    let addresses = parse_addresses(&given)?;
    let count = i64::try_from(addresses.len()).unwrap_or(i64::MAX);
    Ok(Removal {
        all: false,
        search: String::new(),
        addresses,
        count,
    })
}

fn removal_title(removal: &Removal) -> String {
    let count = removal.count;
    match (removal.all, count, removal.search.is_empty()) {
        (false, 1, _) => "Remove 1 address?".to_owned(),
        (false, _, _) => format!("Remove {count} addresses?"),
        (true, _, true) => format!("Remove all {count} addresses?"),
        (true, 1, false) => format!("Remove the 1 address matching “{}”?", removal.search),
        (true, _, false) => format!(
            "Remove the {count} addresses matching “{}”?",
            removal.search
        ),
    }
}

pub(super) async fn confirm(State(app): State<App>, body: Bytes) -> Response {
    let form = form(&body);
    let removal = match parse_removal(&app, &form) {
        Ok(removal) => removal,
        Err(Refusal::User(error)) => {
            return render(&Confirm {
                confirm: ConfirmBody {
                    error,
                    ..ConfirmBody::default()
                },
            });
        }
        Err(Refusal::Internal(err)) => return fail("review removal", &err),
    };

    let shown: Vec<String> = if removal.all {
        match app.db.query_addresses(&AddressQuery {
            search: removal.search.clone(),
            limit: i64::try_from(CONFIRM_SHOWN).unwrap_or(200),
            ..AddressQuery::default()
        }) {
            Ok((found, _)) => found.into_iter().map(|a| a.address).collect(),
            Err(err) => return fail("review removal", &err),
        }
    } else {
        removal
            .addresses
            .iter()
            .take(CONFIRM_SHOWN)
            .cloned()
            .collect()
    };
    let more = usize::try_from(removal.count)
        .unwrap_or(0)
        .saturating_sub(shown.len());
    render(&Confirm {
        confirm: ConfirmBody {
            title: removal_title(&removal),
            shown,
            more,
            selected: removal.addresses.clone(),
            all: removal.all,
            search: removal.search.clone(),
            count: removal.count,
            error: String::new(),
        },
    })
}

pub(super) async fn cancel() -> Response {
    render(&Confirm {
        confirm: ConfirmBody::default(),
    })
}

pub(super) async fn remove(State(app): State<App>, body: Bytes) -> Response {
    let form = form(&body);
    let removal = match parse_removal(&app, &form) {
        Ok(removal) => removal,
        Err(Refusal::User(error)) => {
            return results(
                &app,
                &form,
                Edit {
                    error,
                    clear_confirm: true,
                    ..Edit::default()
                },
            );
        }
        Err(Refusal::Internal(err)) => return fail("remove addresses", &err),
    };

    let removed = if removal.all {
        let expected = field(&form, "count").parse::<i64>().ok();
        if expected != Some(removal.count) {
            let error = "the list changed since you reviewed it; nothing was removed".to_owned();
            return results(
                &app,
                &form,
                Edit {
                    error,
                    clear_confirm: true,
                    ..Edit::default()
                },
            );
        }
        app.db.remove_addresses_matching(&removal.search)
    } else {
        app.db.remove_listed(&removal.addresses)
    };
    match removed {
        Ok(removed) => {
            let note = format!(
                "Removed {removed} address{}",
                if removed == 1 { "" } else { "es" }
            );
            results(
                &app,
                &form,
                Edit {
                    note,
                    clear_confirm: true,
                    ..Edit::default()
                },
            )
        }
        Err(err) if err.is_user_error() => results(
            &app,
            &form,
            Edit {
                error: err.to_string(),
                clear_confirm: true,
                ..Edit::default()
            },
        ),
        Err(err) => fail("remove addresses", &err),
    }
}
