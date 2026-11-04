//! The pages and what they show.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use tracing::warn;

use super::listing::{Column, PAGE_SIZE, SortChoice, TableState, count_label};
use super::{App, Job, fail};
use crate::classify::Label;
use crate::live::Waiting;
use crate::store::{self, Decision, DecisionQuery, DecisionSort};
use crate::{health, rules};

const VERDICTS: [&str; 3] = ["spam", "uncertain", "good"];

pub(super) fn local_time(at: Timestamp) -> String {
    at.to_zoned(TimeZone::system())
        .strftime("%Y-%m-%d %H:%M")
        .to_string()
}

pub(super) fn local_clock(at: Timestamp) -> String {
    at.to_zoned(TimeZone::system())
        .strftime("%H:%M:%S")
        .to_string()
}

pub(super) fn render(template: &impl Template) -> Response {
    match template.render() {
        Ok(body) => Html(body).into_response(),
        Err(err) => fail("render page", &err),
    }
}

pub(super) fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some_and(|v| v == "true")
}

pub(super) fn params(query: Query<BTreeMap<String, String>>) -> BTreeMap<String, String> {
    query.0
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    title: &'static str,
    nav: &'static str,
    version: String,
}

#[derive(Template)]
#[template(path = "enroll.html")]
struct EnrollPage {
    title: &'static str,
    nav: &'static str,
    version: String,
}

pub(super) async fn login(State(app): State<App>) -> Response {
    render(&LoginPage {
        title: "sign in",
        nav: "",
        version: app.options.version.clone(),
    })
}

pub(super) async fn enroll(State(app): State<App>) -> Response {
    render(&EnrollPage {
        title: "enroll a passkey",
        nav: "",
        version: app.options.version.clone(),
    })
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per button or state the row shows"
)]
pub(super) struct DecisionRow {
    pub id: i64,
    pub when: String,
    pub verdict: String,
    pub reason: String,
    pub score: String,
    pub action: String,
    pub folder: String,
    pub from: String,
    pub subject: String,
    pub can_undo: bool,
    pub can_learn: bool,
    pub undone: bool,
    pub undone_by: i64,
    pub corrected_by: i64,
    pub corrects: Vec<i64>,
    pub note: String,
    pub failed: bool,
}

struct VerdictChoice {
    value: &'static str,
    label: &'static str,
    checked: bool,
}

struct DecisionsBody {
    sort: &'static str,
    dir: &'static str,
    search: String,
    rows: Vec<DecisionRow>,
    count: String,
    verdict: String,
    verdicts: Vec<VerdictChoice>,
    columns: Vec<Column>,
    more_href: String,
    only_id: i64,
}

#[derive(Template)]
#[template(path = "decisions.html")]
struct DecisionsPage {
    title: &'static str,
    nav: &'static str,
    version: String,
    body: DecisionsBody,
}

#[derive(Template)]
#[template(path = "decisions_results.html")]
struct DecisionsResults {
    body: DecisionsBody,
}

#[derive(Template)]
#[template(path = "decisions_rows.html")]
struct DecisionsRows {
    body: DecisionsBody,
}

#[derive(Template)]
#[template(path = "decision_row.html")]
struct DecisionRowView {
    row: DecisionRow,
}

const DECISION_SORTS: [SortChoice; 9] = [
    SortChoice {
        key: "time",
        label: "Time",
        asc_first: false,
    },
    SortChoice {
        key: "id",
        label: "ID",
        asc_first: false,
    },
    SortChoice {
        key: "verdict",
        label: "Verdict",
        asc_first: true,
    },
    SortChoice {
        key: "reason",
        label: "Reason",
        asc_first: true,
    },
    SortChoice {
        key: "score",
        label: "Score",
        asc_first: false,
    },
    SortChoice {
        key: "action",
        label: "Action",
        asc_first: true,
    },
    SortChoice {
        key: "folder",
        label: "Folder",
        asc_first: true,
    },
    SortChoice {
        key: "from",
        label: "From",
        asc_first: true,
    },
    SortChoice {
        key: "subject",
        label: "Subject",
        asc_first: true,
    },
];

fn own_judgement(decision: &Decision) -> bool {
    matches!(decision.reason.as_str(), "bayes" | "whitelisted")
}

fn undoable(decision: &Decision) -> bool {
    decision.applied && !decision.undone && own_judgement(decision)
}

fn learnable(decision: &Decision) -> bool {
    decision.corrected_by == 0
        && ((!decision.applied && own_judgement(decision)) || decision.reason == "correction")
}

fn action_label(decision: &Decision) -> String {
    if decision.action != "none" && !decision.applied {
        format!("would {}", decision.action)
    } else {
        decision.action.clone()
    }
}

fn decision_row(decision: &Decision) -> DecisionRow {
    DecisionRow {
        id: decision.id,
        when: local_time(decision.at),
        verdict: decision.verdict.clone(),
        reason: decision.reason.clone(),
        score: format!("{:.2}", decision.score),
        action: action_label(decision),
        folder: decision.mailbox.clone(),
        from: decision.from.clone(),
        subject: decision.subject.clone(),
        can_undo: undoable(decision),
        can_learn: learnable(decision),
        undone: decision.undone,
        undone_by: decision.undone_by,
        corrected_by: decision.corrected_by,
        corrects: decision.corrects.clone(),
        note: String::new(),
        failed: false,
    }
}

pub(super) async fn decisions(
    State(app): State<App>,
    headers: HeaderMap,
    query: Query<BTreeMap<String, String>>,
) -> Response {
    let query = params(query);
    let mut state = TableState::parse("/", &query, &DECISION_SORTS);
    let verdict = query
        .get("verdict")
        .filter(|v| VERDICTS.contains(&v.as_str()))
        .cloned()
        .unwrap_or_default();
    state.filters.insert("verdict", verdict.clone());
    let only_id = query
        .get("id")
        .and_then(|id| id.parse::<i64>().ok())
        .filter(|id| *id >= 1)
        .unwrap_or(0);
    if only_id > 0 {
        state.filters.insert("id", only_id.to_string());
    }

    let (found, total) = match app.db.query_decisions(&DecisionQuery {
        id: only_id,
        verdict: verdict.clone(),
        search: state.search.clone(),
        sort: DecisionSort::from_name(state.sort),
        asc: state.asc,
        offset: state.offset,
        limit: PAGE_SIZE,
    }) {
        Ok(found) => found,
        Err(err) => return fail("list decisions", &err),
    };

    let rows: Vec<DecisionRow> = found.iter().map(decision_row).collect();
    let verdicts = std::iter::once(("", "All"))
        .chain(VERDICTS.iter().map(|v| (*v, *v)))
        .map(|(value, label)| VerdictChoice {
            value,
            label,
            checked: value == verdict,
        })
        .collect();
    let body = DecisionsBody {
        sort: state.sort,
        dir: state.dir(),
        search: state.search.clone(),
        count: count_label(
            total,
            "decision",
            "decisions",
            !verdict.is_empty() || !state.search.is_empty() || only_id > 0,
        ),
        more_href: state.more_href(rows.len(), total),
        columns: state.columns(&DECISION_SORTS),
        rows,
        verdict,
        verdicts,
        only_id,
    };

    if !is_htmx(&headers) {
        return render(&DecisionsPage {
            title: "decisions",
            nav: "decisions",
            version: app.options.version.clone(),
            body,
        });
    }
    if state.offset > 0 {
        render(&DecisionsRows { body })
    } else {
        render(&DecisionsResults { body })
    }
}

async fn run_job(app: &App, id: i64, job: impl FnOnce(i64) -> Job) -> Response {
    match app.db.decision(id) {
        Err(store::Error::NotFound(_)) => {
            return (StatusCode::NOT_FOUND, "no such decision").into_response();
        }
        Err(err) => return fail("find decision", &err),
        Ok(_) => {}
    }

    let outcome = {
        let _one_at_a_time = app.action_lock.lock().await;
        match &app.options.run {
            Some(run) => run(job(id)).await,
            None => Err("changing decisions needs the daemon: use `tamis run`".to_owned()),
        }
    };
    if let Err(err) = &outcome {
        warn!(decision = id, %err, "decision action failed");
    }

    let decision = match app.db.decision(id) {
        Ok(decision) => decision,
        Err(err) => return fail("reload decision", &err),
    };
    let mut row = decision_row(&decision);
    row.failed = outcome.is_err();
    row.note = outcome.unwrap_or_else(|err| err);
    render(&DecisionRowView { row })
}

pub(super) async fn undo(State(app): State<App>, Path(id): Path<String>) -> Response {
    match id.parse() {
        Ok(id) => run_job(&app, id, Job::Undo).await,
        Err(_) => (StatusCode::BAD_REQUEST, "bad decision id").into_response(),
    }
}

pub(super) async fn learn(
    State(app): State<App>,
    Path(id): Path<String>,
    query: Query<BTreeMap<String, String>>,
) -> Response {
    let label = match query.get("label").map(String::as_str) {
        None | Some("") => None,
        Some("good") => Some(Label::Good),
        Some("spam") => Some(Label::Spam),
        Some(_) => return (StatusCode::BAD_REQUEST, "label must be good or spam").into_response(),
    };
    match id.parse() {
        Ok(id) => run_job(&app, id, |id| Job::Learn(id, label)).await,
        Err(_) => (StatusCode::BAD_REQUEST, "bad decision id").into_response(),
    }
}

struct CheckRow {
    status: &'static str,
    name: String,
    detail: String,
}

struct DaemonView {
    started: String,
    last_pass: String,
    summary: String,
    doing: String,
    passes: u32,
    last_error: String,
    recovered: bool,
}

struct HealthBody {
    checks: Vec<CheckRow>,
    daemon: Option<DaemonView>,
}

#[derive(Template)]
#[template(path = "health.html")]
struct HealthPage {
    title: &'static str,
    nav: &'static str,
    version: String,
    body: HealthBody,
}

fn daemon_view(app: &App) -> Option<DaemonView> {
    let snap = app.options.status.as_ref()?.snapshot();
    let mut last_error = String::new();
    let mut recovered = false;
    if !snap.last_error.is_empty() {
        last_error = format!(
            "{}: {}",
            snap.last_error_at.map_or_else(String::new, local_time),
            snap.last_error
        );
    }
    match (snap.failures, snap.retry_at, snap.recovered_at) {
        (failures, Some(retry), _) if failures > 0 => {
            let _ = write!(
                last_error,
                " (failed {failures} in a row since {}; next try {})",
                snap.failing_since.map_or_else(String::new, local_clock),
                local_clock(retry)
            );
        }
        (_, _, Some(at)) if !snap.last_error.is_empty() => {
            let _ = write!(last_error, " (recovered {})", local_clock(at));
            recovered = true;
        }
        _ => {}
    }
    Some(DaemonView {
        started: snap.started.map_or_else(String::new, local_time),
        last_pass: snap
            .last_pass
            .map_or_else(|| "no pass yet".to_owned(), local_time),
        summary: snap.last_summary,
        doing: match snap.waiting {
            Some(Waiting::Idle) => "idle",
            Some(Waiting::Polling) => "polling",
            Some(Waiting::Running) => "running",
            None => "",
        }
        .to_owned(),
        passes: snap.passes,
        last_error,
        recovered,
    })
}

pub(super) async fn health(State(app): State<App>) -> Response {
    let report = match health::collect(&app.db, &app.cfg) {
        Ok(report) => report,
        Err(err) => return fail("check health", &err),
    };
    let checks = report
        .checks
        .iter()
        .map(|c| CheckRow {
            status: c.status.as_str(),
            name: c.name.clone(),
            detail: c.detail.clone(),
        })
        .collect();
    render(&HealthPage {
        title: "health",
        nav: "health",
        version: app.options.version.clone(),
        body: HealthBody {
            checks,
            daemon: daemon_view(&app),
        },
    })
}

struct RulesBody {
    path: String,
    source: String,
    problem: String,
}

#[derive(Template)]
#[template(path = "rules.html")]
struct RulesPage {
    title: &'static str,
    nav: &'static str,
    version: String,
    body: RulesBody,
}

#[derive(Default)]
struct RulesResult {
    problem: String,
    folder: String,
}

#[derive(Template)]
#[template(path = "rules_result.html")]
struct RulesResultView {
    result: RulesResult,
}

pub(super) async fn rules(State(app): State<App>) -> Response {
    let path = app.cfg.rules.script.clone();
    let mut body = RulesBody {
        path: path.clone(),
        source: String::new(),
        problem: String::new(),
    };
    if !path.is_empty() {
        match std::fs::read_to_string(&path) {
            Ok(source) => {
                body.source = source;
                if let Err(err) = rules::Rules::load(std::path::Path::new(&path)) {
                    body.problem = err.to_string();
                }
            }
            Err(err) => body.problem = err.to_string(),
        }
    }
    render(&RulesPage {
        title: "rules",
        nav: "rules",
        version: app.options.version.clone(),
        body,
    })
}

pub(super) async fn rules_test(
    State(app): State<App>,
    Form(form): Form<BTreeMap<String, String>>,
) -> Response {
    let script = &app.cfg.rules.script;
    let message = form.get("message").map_or("", String::as_str);
    let mut result = RulesResult::default();
    if script.is_empty() {
        "No rules script configured (rules.script).".clone_into(&mut result.problem);
    } else if message.is_empty() {
        "Paste a message, headers included.".clone_into(&mut result.problem);
    } else {
        let raw = message.as_bytes();
        match rules::Rules::load(std::path::Path::new(script))
            .and_then(|loaded| loaded.folder(raw, &rules::sender_of(raw)))
        {
            Ok(folder) => result.folder = folder.unwrap_or_default(),
            Err(err) => result.problem = err.to_string(),
        }
    }
    render(&RulesResultView { result })
}

pub(super) struct LogRow {
    pub when: String,
    pub level: String,
    pub text: String,
}

#[derive(Default)]
pub(super) struct DebugView {
    pub available: bool,
    pub locked: bool,
    pub on: bool,
    pub until: String,
    pub span: String,
}

#[derive(Default)]
pub(super) struct LogsBody {
    pub rows: Vec<LogRow>,
    pub debug: DebugView,
}

#[derive(Template)]
#[template(path = "logs.html")]
struct LogsPage {
    title: &'static str,
    nav: &'static str,
    version: String,
    body: LogsBody,
}

#[derive(Template)]
#[template(path = "logs_entries.html")]
struct LogsEntries {
    body: LogsBody,
}

fn debug_view(app: &App) -> DebugView {
    let Some(debug) = &app.options.debug else {
        return DebugView::default();
    };
    let (on, until) = debug.status();
    DebugView {
        available: true,
        locked: debug.locked(),
        on,
        until: until.filter(|_| on).map_or_else(String::new, local_clock),
        span: minutes(debug.span()),
    }
}

fn minutes(span: std::time::Duration) -> String {
    let count = span.as_secs().div_ceil(60);
    if count == 1 {
        "1 minute".to_owned()
    } else {
        format!("{count} minutes")
    }
}

fn logs_body(app: &App) -> LogsBody {
    let rows = app
        .options
        .logs
        .as_ref()
        .map(|ring| {
            ring.entries()
                .into_iter()
                .map(|entry| LogRow {
                    when: local_clock(entry.at),
                    level: entry.level.to_string(),
                    text: entry.text,
                })
                .collect()
        })
        .unwrap_or_default();
    LogsBody {
        rows,
        debug: debug_view(app),
    }
}

fn render_logs(app: &App, entries_only: bool) -> Response {
    let body = logs_body(app);
    if entries_only {
        return render(&LogsEntries { body });
    }
    render(&LogsPage {
        title: "logs",
        nav: "logs",
        version: app.options.version.clone(),
        body,
    })
}

pub(super) async fn logs(State(app): State<App>, headers: HeaderMap) -> Response {
    render_logs(&app, is_htmx(&headers))
}

pub(super) async fn set_debug(
    State(app): State<App>,
    query: Query<BTreeMap<String, String>>,
) -> Response {
    let Some(debug) = &app.options.debug else {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "not found",
        )
            .into_response();
    };
    match query.get("state").map(String::as_str) {
        Some("on") => debug.on(),
        Some("off") => debug.off(),
        _ => {}
    }
    render_logs(&app, true)
}
