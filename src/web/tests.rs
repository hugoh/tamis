#![allow(clippy::unwrap_used, reason = "a failing call should fail the test")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderValue, Request, StatusCode, header};
use axum::response::Response;
use http_body_util::BodyExt;
use jiff::Timestamp;
use tower::ServiceExt;

use super::{App, Job, Options, RunJobs, SESSION_COOKIE, build, hash_token};
use crate::config::Config;
use crate::live::{Status, Waiting};
use crate::store::{Decision, Store};

const ORIGIN: &str = "http://localhost:8484";

struct Site {
    router: Router,
    app: App,
    db: Arc<Store>,
    session: String,
    dir: tempfile::TempDir,
}

fn site_with(options: Options) -> Site {
    site_with_rules(options, "")
}

fn site_with_rules(options: Options, rules: &str) -> Site {
    site_at(ORIGIN, options, rules)
}

fn site_at(origin: &str, options: Options, rules: &str) -> Site {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let script = dir.path().join("rules.sieve");
    let rules_section = if rules.is_empty() {
        String::new()
    } else {
        std::fs::write(&script, rules).unwrap();
        format!("[rules]\nscript = \"{}\"\n", script.display())
    };
    std::fs::write(
        &config,
        format!(
            "state = \"{}\"\n[account]\nhost = \"imap.example.org\"\nuser = \"me@example.org\"\n[web]\nenabled = true\norigin = \"{origin}\"\n{rules_section}",
            dir.path().join("state.db").display()
        ),
    )
    .unwrap();
    let cfg = Arc::new(Config::load(&|_| None, &config).unwrap());
    let db = Arc::new(Store::open(&dir.path().join("state.db")).unwrap());
    let (router, app) = build(cfg, Arc::clone(&db), options).unwrap();
    db.add_passkey("session-key", "test", &[]).unwrap();
    let session = app
        .sessions
        .put("session-key".to_owned(), std::time::Duration::from_secs(60))
        .unwrap();
    Site {
        router,
        app,
        db,
        session,
        dir,
    }
}

fn site() -> Site {
    site_with(Options::default())
}

impl Site {
    async fn send(&self, mut request: Request<Body>, signed_in: bool) -> Response {
        if signed_in {
            let cookie = format!("{SESSION_COOKIE}={}", self.session);
            request
                .headers_mut()
                .insert(header::COOKIE, HeaderValue::from_str(&cookie).unwrap());
        }
        self.router.clone().oneshot(request).await.unwrap()
    }

    async fn get(&self, uri: &str) -> Response {
        self.send(Request::get(uri).body(Body::empty()).unwrap(), true)
            .await
    }

    async fn get_htmx(&self, uri: &str) -> Response {
        self.send(
            Request::get(uri)
                .header("hx-request", "true")
                .body(Body::empty())
                .unwrap(),
            true,
        )
        .await
    }

    async fn post(&self, uri: &str, body: &str) -> Response {
        let request = Request::post(uri)
            .header(header::ORIGIN, ORIGIN)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_owned()))
            .unwrap();
        self.send(request, true).await
    }
}

async fn text(response: Response) -> String {
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

fn record(site: &Site, subject: &str, reason: &str, applied: bool) -> i64 {
    record_as(site, subject, ("spam", 0.97), reason, "move:Junk", applied)
}

fn record_as(
    site: &Site,
    subject: &str,
    (verdict, score): (&str, f64),
    reason: &str,
    action: &str,
    applied: bool,
) -> i64 {
    site.db
        .record_decision(&Decision {
            at: Timestamp::now(),
            mailbox: "INBOX".into(),
            uid: 1,
            message_id: format!("<{subject}@x>"),
            from: "alice@example.org".into(),
            subject: subject.into(),
            score,
            verdict: verdict.parse().unwrap(),
            reason: reason.parse().unwrap(),
            action: action.parse().unwrap(),
            applied,
            ..Decision::default()
        })
        .unwrap()
}

#[tokio::test]
async fn pages_need_a_session() {
    let site = site();

    let page = site
        .send(Request::get("/").body(Body::empty()).unwrap(), false)
        .await;
    let change = site
        .send(
            Request::post("/whitelist")
                .header(header::ORIGIN, ORIGIN)
                .body(Body::empty())
                .unwrap(),
            false,
        )
        .await;

    assert_eq!(
        (
            page.status(),
            page.headers()[header::LOCATION].to_str().unwrap()
        ),
        (StatusCode::SEE_OTHER, "/auth/login")
    );
    assert_eq!(change.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn changes_from_another_origin_are_refused_even_when_signed_in() {
    let site = site();
    for origin in [Some("https://evil.example"), None] {
        let mut request = Request::post("/whitelist")
            .body(Body::from("address=a@b.org"))
            .unwrap();
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert(header::ORIGIN, HeaderValue::from_static(origin));
        }

        let response = site.send(request, true).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{origin:?}");
    }
    assert!(site.db.addresses().unwrap().is_empty());
}

#[tokio::test]
async fn every_response_carries_the_security_headers() {
    let site = site();

    let response = site
        .send(
            Request::get("/auth/login").body(Body::empty()).unwrap(),
            false,
        )
        .await;

    let headers = response.headers();
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .starts_with("default-src 'self'")
    );
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert!(text(response).await.contains("Sign in with a passkey"));
}

#[tokio::test]
async fn the_decisions_page_lists_filters_and_escapes() {
    let site = site();
    record(&site, "<script>alert(1)</script>", "bayes", true);
    record(&site, "lunch", "bayes", true);

    let page = text(site.get("/").await).await;
    let found = text(site.get_htmx("/?q=lunch").await).await;

    assert!(
        page.contains("<title>tamis · decisions</title>") && page.contains("2 decisions"),
        "{page}"
    );
    assert!(
        page.contains("&#60;script&#62;") && !page.contains("<script>alert"),
        "{page}"
    );
    assert!(
        found.starts_with("<div id=\"results\">")
            && found.contains("1 decision match")
            && !found.contains("script&#62;"),
        "{found}"
    );
}

#[tokio::test]
async fn the_decisions_list_says_when_it_was_generated() {
    let site = site();
    record(&site, "lunch", "bayes", true);

    let page = text(site.get("/").await).await;
    let refreshed = text(site.get_htmx("/").await).await;

    assert!(page.contains("Updated "), "{page}");
    assert!(
        refreshed.starts_with("<div id=\"results\">") && refreshed.contains("Updated "),
        "{refreshed}"
    );
}

#[tokio::test]
async fn a_rule_decision_gets_a_second_pill_and_a_move_shows_its_destination() {
    let site = site();
    record(&site, "filed", "rule", true);
    record(&site, "plain", "bayes", true);

    let page = text(site.get("/").await).await;

    assert_eq!(page.matches("badge-info").count(), 1, "{page}");
    assert!(page.contains("INBOX → Junk"), "{page}");
}

#[tokio::test]
async fn decisions_can_be_filtered_by_reason() {
    let site = site();
    record(&site, "by-rule", "rule", true);
    record(&site, "by-bayes", "bayes", true);

    let page = text(site.get("/").await).await;
    let found = text(site.get_htmx("/?reason=rule").await).await;

    assert!(
        page.contains("name=\"reason\"") && page.contains("value=\"whitelisted\""),
        "{page}"
    );
    assert!(
        found.contains("1 decision match")
            && found.contains("by-rule")
            && !found.contains("by-bayes"),
        "{found}"
    );
}

/// Writes a standalone decisions page (CSS inlined) for eyeballing or docs:
/// `mise run mock-web`; set `TAMIS_MOCK_PAGE` to write elsewhere
#[tokio::test]
#[ignore = "writes a file; run by hand"]
async fn mock_decisions_page() {
    let site = site();
    record_as(
        &site,
        "Your receipt from Acme",
        ("good", 0.03),
        "rule",
        "move:Receipts",
        true,
    );
    record_as(
        &site,
        "Weekly digest",
        ("good", 0.08),
        "rule",
        "move:Newsletters",
        true,
    );
    record_as(
        &site,
        "Meeting notes",
        ("good", 0.02),
        "bayes",
        "none",
        true,
    );
    record_as(
        &site,
        "Newsletter",
        ("good", 0.0),
        "whitelisted",
        "none",
        true,
    );
    record_as(
        &site,
        "Is this you?",
        ("uncertain", 0.52),
        "bayes",
        "none",
        true,
    );
    record_as(
        &site,
        "You won a prize!!!",
        ("spam", 0.99),
        "bayes",
        "move:Junk",
        true,
    );
    let rescued = record_as(
        &site,
        "Lunch on Friday?",
        ("spam", 0.91),
        "bayes",
        "move:Junk",
        true,
    );
    site.db.mark_undone(rescued).unwrap();

    let page = text(site.get("/").await).await;
    let css =
        std::fs::read_to_string("web/static/vendor/theme.css").expect("run mise run vendor-web");
    let page = page
        .replace(
            "<link rel=\"stylesheet\" href=\"/static/vendor/theme.css?v=\">",
            &format!("<style>{css}</style>"),
        )
        .replace(
            "<script src=\"/static/vendor/htmx.min.js?v=\"></script>",
            "",
        );
    let out =
        std::env::var("TAMIS_MOCK_PAGE").unwrap_or_else(|_| "target/mock-decisions.html".into());
    std::fs::write(out, page).unwrap();
}

#[tokio::test]
async fn a_decision_offers_undo_only_when_it_can_be_undone() {
    let site = site();
    let applied = record(&site, "applied", "bayes", true);
    let shadow = record(&site, "shadow", "bayes", false);

    let applied_row = text(site.get_htmx(&format!("/?id={applied}")).await).await;
    let shadow_row = text(site.get_htmx(&format!("/?id={shadow}")).await).await;

    assert!(
        applied_row.contains("/undo\"") && !applied_row.contains("label=spam"),
        "{applied_row}"
    );
    assert!(
        shadow_row.contains("would move:Junk")
            && shadow_row.contains("label=spam")
            && !shadow_row.contains("/undo\""),
        "{shadow_row}"
    );
}

#[tokio::test]
async fn undo_runs_the_job_and_shows_what_it_did() {
    let jobs = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&jobs);
    let run: RunJobs = Arc::new(move |job| {
        seen.lock().unwrap().push(format!("{job:?}"));
        Box::pin(async { Ok("moved back to INBOX".to_owned()) })
    });
    let site = site_with(Options {
        run: Some(run),
        ..Options::default()
    });
    let id = record(&site, "oops", "bayes", true);

    let response = site.post(&format!("/decisions/{id}/undo"), "").await;
    let row = text(response).await;

    assert_eq!(jobs.lock().unwrap().as_slice(), [format!("Undo({id})")]);
    assert!(
        row.contains("moved back to INBOX") && row.contains("text-success"),
        "{row}"
    );
}

#[tokio::test]
async fn a_failed_action_says_so_and_a_missing_decision_is_not_found() {
    let run: RunJobs = Arc::new(|_| Box::pin(async { Err("the mailbox moved on".to_owned()) }));
    let site = site_with(Options {
        run: Some(run),
        ..Options::default()
    });
    let id = record(&site, "oops", "bayes", false);

    let failed = text(
        site.post(&format!("/decisions/{id}/learn?label=good"), "")
            .await,
    )
    .await;
    let missing = site.post("/decisions/999/undo", "").await;
    let bad_label = site
        .post(&format!("/decisions/{id}/learn?label=maybe"), "")
        .await;
    let bad_id = site.post("/decisions/x/undo", "").await;

    assert!(
        failed.contains("the mailbox moved on") && failed.contains("text-error"),
        "{failed}"
    );
    assert_eq!(
        (missing.status(), bad_label.status(), bad_id.status()),
        (
            StatusCode::NOT_FOUND,
            StatusCode::BAD_REQUEST,
            StatusCode::BAD_REQUEST
        )
    );
}

#[tokio::test]
async fn learning_passes_the_chosen_label_to_the_job() {
    let jobs = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&jobs);
    let run: RunJobs = Arc::new(move |job: Job| {
        seen.lock().unwrap().push(format!("{job:?}"));
        Box::pin(async { Ok(String::new()) })
    });
    let site = site_with(Options {
        run: Some(run),
        ..Options::default()
    });
    let id = record(&site, "x", "bayes", false);

    site.post(&format!("/decisions/{id}/learn?label=spam"), "")
        .await;
    site.post(&format!("/decisions/{id}/learn"), "").await;

    assert_eq!(
        jobs.lock().unwrap().as_slice(),
        [
            format!("Learn({id}, Some(Spam))"),
            format!("Learn({id}, None)")
        ]
    );
}

#[tokio::test]
async fn the_whitelist_adds_searches_and_removes() {
    let site = site();

    let added = text(
        site.post("/whitelist", "address=Alice+%3Calice%40Example.org%3E")
            .await,
    )
    .await;
    let rejected = text(site.post("/whitelist", "address=not+an+address").await).await;
    site.db.add_address("bob@example.org", "learned").unwrap();
    let found = text(site.get_htmx("/whitelist?q=bob").await).await;

    assert!(
        added.contains("1 address") && added.contains("alice@example.org"),
        "{added}"
    );
    assert!(rejected.contains("alert-error"), "{rejected}");
    assert!(
        found.contains("bob@example.org") && !found.contains("alice@example.org"),
        "{found}"
    );

    let confirm = text(
        site.post("/whitelist/remove/confirm", "address=alice%40example.org")
            .await,
    )
    .await;
    assert!(confirm.contains("Remove 1 address?"), "{confirm}");
    let removed = text(
        site.post("/whitelist/remove", "address=alice%40example.org")
            .await,
    )
    .await;
    assert!(
        removed.contains("Removed 1 address") && !removed.contains("alice@example.org"),
        "{removed}"
    );
}

#[tokio::test]
async fn removing_every_match_checks_the_list_has_not_changed() {
    let site = site();
    for name in ["a", "b", "c"] {
        site.db
            .add_address(&format!("{name}@example.org"), "manual")
            .unwrap();
    }

    let stale = text(site.post("/whitelist/remove", "scope=all&q=&count=2").await).await;
    assert!(
        stale.contains("the list changed") && site.db.addresses().unwrap().len() == 3,
        "{stale}"
    );

    let confirm = text(site.post("/whitelist/remove/confirm", "scope=all&q=").await).await;
    assert!(confirm.contains("Remove all 3 addresses?"), "{confirm}");
    let done = text(site.post("/whitelist/remove", "scope=all&q=&count=3").await).await;
    assert!(
        done.contains("Removed 3 addresses") && site.db.addresses().unwrap().is_empty(),
        "{done}"
    );

    let nothing = text(site.post("/whitelist/remove/confirm", "").await).await;
    assert!(nothing.contains("select at least one address"), "{nothing}");
}

#[tokio::test]
async fn health_and_rules_pages_render() {
    let site = site();

    let health = text(site.get("/health").await).await;
    let rules = text(site.get("/rules").await).await;
    let tried = text(
        site.post("/rules/test", "message=From%3A+a%40b.org%0D%0A%0D%0Ahi")
            .await,
    )
    .await;
    let logs = text(site.get("/logs").await).await;

    assert!(
        health.contains("<h1 class=\"text-2xl font-semibold\">Health</h1>"),
        "{health}"
    );
    assert!(rules.contains("No rules script configured"), "{rules}");
    assert!(
        tried.contains("No rules script configured (rules.script)."),
        "{tried}"
    );
    assert!(
        logs.contains("No log lines") && !logs.contains("Debug logging"),
        "{logs}"
    );
}

#[tokio::test]
async fn enrolling_needs_a_link_that_works_once() {
    let site = site();
    site.db
        .add_enrollment(
            &hash_token("secret-link"),
            Timestamp::now() + std::time::Duration::from_secs(300),
        )
        .unwrap();
    let begin = |token: &str| {
        let request = Request::post("/auth/register/begin")
            .header(header::ORIGIN, ORIGIN)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({ "token": token }).to_string(),
            ))
            .unwrap();
        site.send(request, false)
    };

    let wrong = begin("guess").await;
    let first = begin("secret-link").await;
    let reused = begin("secret-link").await;

    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(first.status(), StatusCode::OK);
    assert!(
        first.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("HttpOnly; SameSite=Strict")
    );
    let options = text(first).await;
    assert!(
        options.contains("\"challenge\"") && options.contains("\"requireResidentKey\":true"),
        "{options}"
    );
    assert_eq!(reused.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn finishing_a_ceremony_that_was_never_started_is_refused() {
    let site = site();
    let request = Request::post("/auth/login/finish")
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();

    let response = site.send(request, false).await;

    assert!(response.status().is_client_error());
    assert_eq!(site.app.sessions.take("nothing"), None);
    assert_eq!(
        site.post("/logs/debug?state=on", "").await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn signing_out_ends_the_session() {
    let site = site();

    let response = site.post("/auth/logout", "").await;
    let after = site.get("/").await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(after.status(), StatusCode::SEE_OTHER);
}

fn logging(locked: bool) -> (Options, Arc<crate::live::Ring>) {
    let ring = Arc::new(crate::live::Ring::new(10));
    let debug = Arc::new(crate::live::DebugSwitch::new(
        locked,
        std::time::Duration::from_mins(15),
        Arc::clone(&ring),
        |_| {},
    ));
    (
        Options {
            logs: Some(Arc::clone(&ring)),
            debug: Some(debug),
            ..Options::default()
        },
        ring,
    )
}

fn log_line(ring: &crate::live::Ring, text: &str) {
    ring.add(crate::live::Entry {
        at: Timestamp::now(),
        level: tracing::Level::WARN,
        text: text.to_owned(),
    });
}

#[tokio::test]
async fn the_debug_switch_turns_logging_on_and_off_from_the_logs_page() {
    let (options, ring) = logging(false);
    let site = site_with(options);

    let off = text(site.get("/logs").await).await;
    let on = text(site.post("/logs/debug?state=on", "").await).await;
    log_line(&ring, "<b>pass failed</b> uid=7");
    let shown = text(site.get_htmx("/logs").await).await;
    let back = text(site.post("/logs/debug?state=off", "").await).await;

    assert!(
        off.contains("Debug logging off") && off.contains("Turn on for 15 minutes"),
        "{off}"
    );
    assert!(
        on.contains("Debug logging on until") && on.contains("Turn off"),
        "{on}"
    );
    assert!(
        shown.contains("&#60;b&#62;pass failed") && shown.contains("badge-warning"),
        "{shown}"
    );
    assert!(
        back.contains("Debug logging off") && back.contains("No log lines"),
        "{back}"
    );
}

#[tokio::test]
async fn a_verbose_daemon_shows_that_debug_is_locked_on() {
    let (options, ring) = logging(true);
    let site = site_with(options);
    log_line(&ring, "idle mailbox=INBOX");

    let page = text(site.get("/logs").await).await;

    assert!(
        page.contains("the daemon runs with --verbose") && page.contains("idle mailbox=INBOX"),
        "{page}"
    );
    assert!(!page.contains("Turn on for"), "{page}");
}

#[tokio::test]
async fn revoking_a_passkey_ends_its_sessions() {
    let site = site();
    assert_eq!(site.get("/").await.status(), StatusCode::OK);

    let key = site.db.passkeys().unwrap().remove(0);
    site.db.remove_passkey(key.id).unwrap();

    assert_eq!(site.get("/").await.status(), StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn the_rules_page_hands_its_script_to_the_highlighter() {
    let site = site_with_rules(
        Options::default(),
        "require \"fileinto\";\nfileinto \"A\";\n",
    );

    let page = text(site.get("/rules").await).await;
    let script = site.get("/static/rules.js").await;

    assert!(
        page.contains("<pre id=\"rules-source\"") && page.contains("/static/rules.js?v="),
        "{page}"
    );
    assert_eq!(script.status(), StatusCode::OK);
    assert!(
        script.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .contains("javascript")
    );
}

#[tokio::test]
async fn static_files_carry_an_etag_and_answer_a_matching_one_with_not_modified() {
    let site = site();

    let first = site.get("/static/rules.js").await;
    let etag = first.headers()[header::ETAG].to_str().unwrap().to_owned();
    let revalidate = |tag: String| {
        let request = Request::get("/static/rules.js")
            .header(header::IF_NONE_MATCH, tag)
            .body(Body::empty())
            .unwrap();
        site.send(request, true)
    };
    let same = revalidate(etag.clone()).await;
    let weakened = revalidate(format!("W/{etag}")).await;
    let other = revalidate("\"stale\"".to_owned()).await;

    assert_eq!(first.headers()[header::CACHE_CONTROL], "no-cache");
    assert_eq!(same.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(same.headers()[header::ETAG].to_str().unwrap(), etag);
    assert!(text(same).await.is_empty());
    assert_eq!(weakened.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(other.status(), StatusCode::OK);
}

#[tokio::test]
async fn responses_are_compressed_for_clients_that_accept_it() {
    let site = site();
    let get = |uri: &'static str, encoding: &'static str| {
        let request = Request::get(uri)
            .header(header::ACCEPT_ENCODING, encoding)
            .body(Body::empty())
            .unwrap();
        site.send(request, true)
    };

    let script = get("/static/rules.js", "gzip").await;
    let page = get("/health", "br, gzip").await;
    let plain = get("/static/rules.js", "identity").await;

    assert_eq!(script.headers()[header::CONTENT_ENCODING], "gzip");
    assert_eq!(page.headers()[header::CONTENT_ENCODING], "br");
    assert!(!plain.headers().contains_key(header::CONTENT_ENCODING));
}

#[tokio::test]
async fn versioned_asset_urls_are_cached_for_good_and_bare_ones_are_revalidated() {
    let site = site_with(Options {
        version: "1.2.3".to_owned(),
        ..Options::default()
    });

    let page = text(site.get("/").await).await;
    let login = text(
        site.send(
            Request::get("/auth/login").body(Body::empty()).unwrap(),
            false,
        )
        .await,
    )
    .await;
    let versioned = site.get("/static/rules.js?v=1.2.3").await;
    let bare = site.get("/static/rules.js").await;
    let empty = site.get("/static/rules.js?v=").await;

    for asset in ["vendor/theme.css", "vendor/htmx.min.js", "feedback.js"] {
        assert!(
            page.contains(&format!("/static/{asset}?v=1.2.3\"")),
            "{page}"
        );
    }
    assert!(login.contains("/static/webauthn.js?v=1.2.3\""), "{login}");
    assert_eq!(
        versioned.headers()[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(bare.headers()[header::CACHE_CONTROL], "no-cache");
    assert_eq!(empty.headers()[header::CACHE_CONTROL], "no-cache");
}

#[tokio::test]
async fn pages_load_the_feedback_script_and_a_toast_area() {
    let site = site();

    let page = text(site.get("/").await).await;
    let script = site.get("/static/feedback.js").await;

    assert!(
        page.contains("id=\"toasts\"") && page.contains("/static/feedback.js?v="),
        "{page}"
    );
    assert_eq!(script.status(), StatusCode::OK);
}

#[tokio::test]
async fn an_action_note_is_marked_for_the_toast() {
    let run: RunJobs = Arc::new(|_| Box::pin(async { Err("the mailbox moved on".to_owned()) }));
    let site = site_with(Options {
        run: Some(run),
        ..Options::default()
    });
    let id = record(&site, "oops", "bayes", true);

    let row = text(site.post(&format!("/decisions/{id}/undo"), "").await).await;

    assert!(row.contains("data-note data-failed"), "{row}");
}

#[tokio::test]
async fn decisions_and_health_offer_a_refresh_that_does_not_reload_the_page() {
    let site = site();

    let decisions = text(site.get("/").await).await;
    let health = text(site.get("/health").await).await;

    assert!(
        decisions.contains("<button type=\"submit\" class=\"btn btn-sm\">Refresh</button>"),
        "{decisions}"
    );
    assert!(
        health.contains("hx-get=\"/health\" hx-select=\"main\" hx-target=\"main\""),
        "{health}"
    );
}

#[tokio::test]
async fn an_enrolment_token_in_the_query_string_is_not_accepted() {
    let site = site();
    site.db
        .add_enrollment(
            &hash_token("secret-link"),
            Timestamp::now() + std::time::Duration::from_secs(300),
        )
        .unwrap();
    let request = Request::post("/auth/register/begin?token=secret-link")
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();

    let response = site.send(request, false).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn https_sites_ask_browsers_to_stay_on_https() {
    let secure = site_at("https://tamis.example.org", Options::default(), "");
    let plain = site();

    let over_https = secure.get("/").await;
    let over_http = plain.get("/").await;

    assert!(
        over_https.headers()[header::STRICT_TRANSPORT_SECURITY]
            .to_str()
            .unwrap()
            .starts_with("max-age=")
    );
    assert!(
        !over_http
            .headers()
            .contains_key(header::STRICT_TRANSPORT_SECURITY)
    );
}

#[tokio::test]
async fn a_database_error_while_checking_the_session_signs_nobody_in() {
    let site = site();
    assert_eq!(site.get("/").await.status(), StatusCode::OK);
    rusqlite::Connection::open(site.dir.path().join("state.db"))
        .unwrap()
        .execute_batch("DROP TABLE passkeys")
        .unwrap();

    let response = site.get("/").await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
}

fn ceremony_id(response: &Response) -> String {
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    let (_, rest) = cookie.split_once('=').unwrap();
    rest.split(';').next().unwrap().to_owned()
}

#[tokio::test]
async fn flooding_login_begin_evicts_a_ceremony_in_flight() {
    let site = site();
    let begin = || {
        let request = Request::post("/auth/login/begin")
            .header(header::ORIGIN, ORIGIN)
            .body(Body::empty())
            .unwrap();
        site.send(request, false)
    };
    let victim = ceremony_id(&begin().await);
    for _ in 0..super::token::MAX_ITEMS {
        assert_eq!(begin().await.status(), StatusCode::OK);
    }

    assert!(site.app.ceremonies.take(&victim).is_none());
}

#[tokio::test]
async fn the_decisions_list_pages_on_after_the_last_row_shown() {
    let site = site();
    for n in 0..=100 {
        record(&site, &format!("mail {n:03}"), "bayes", true);
    }

    let first = text(site.get_htmx("/").await).await;
    let more = first
        .split("hx-get=\"")
        .filter_map(|rest| rest.split('"').next())
        .find(|link| link.contains("after="))
        .unwrap()
        .replace("&#38;", "&");
    let second = text(site.get_htmx(&more).await).await;

    assert!(more.contains("after="), "{more}");
    assert!(
        first.contains("mail 100") && !first.contains("mail 000"),
        "{first}"
    );
    assert!(
        second.contains("mail 000") && !second.contains("mail 100"),
        "{second}"
    );
    assert!(!second.contains("after="), "{second}");
}

#[tokio::test]
async fn the_whitelist_pages_on_after_the_last_address_shown() {
    let site = site();
    for n in 0..=100 {
        site.db
            .add_address(&format!("a{n:03}@example.org"), "manual")
            .unwrap();
    }

    let first = text(site.get_htmx("/whitelist").await).await;
    let more = first
        .split("hx-get=\"")
        .filter_map(|rest| rest.split('"').next())
        .find(|link| link.contains("after="))
        .unwrap()
        .replace("&#38;", "&");
    let second = text(site.get_htmx(&more).await).await;

    assert!(
        first.contains("a099@example.org") && !first.contains("a100@example.org"),
        "{first}"
    );
    assert!(
        second.contains("a100@example.org") && !second.contains("a099@example.org"),
        "{second}"
    );
    assert!(!second.contains("after="), "{second}");
}

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).unwrap()
}

async fn health_with(status: Status) -> String {
    let site = site_with(Options {
        status: Some(Arc::new(status)),
        ..Options::default()
    });
    text(site.get("/health").await).await
}

#[tokio::test]
async fn health_shows_no_daemon_section_without_a_daemon() {
    let health = text(site().get("/health").await).await;

    assert!(!health.contains("Last pass"), "{health}");
}

#[tokio::test]
async fn health_shows_a_daemon_that_has_not_passed_yet() {
    let health = health_with(Status::new(at(1_000_000))).await;

    assert!(health.contains("nothing yet"), "{health}");
    assert!(health.contains("no pass yet"), "{health}");
    assert!(!health.contains("Last error"), "{health}");
}

#[tokio::test]
async fn health_shows_what_the_daemon_is_doing_and_its_last_pass() {
    for (waiting, label) in [
        (Waiting::Idle, "idle"),
        (Waiting::Polling, "polling"),
        (Waiting::Running, "running"),
    ] {
        let status = Status::new(at(1_000_000));
        status.pass_done(at(1_000_100), "moved 3 messages");
        status.wait(waiting);

        let health = health_with(status).await;

        assert!(health.contains(&format!(">{label}<")), "{label}: {health}");
        assert!(health.contains("moved 3 messages"), "{label}: {health}");
        assert!(!health.contains("no pass yet"), "{label}: {health}");
    }
}

#[tokio::test]
async fn health_shows_how_long_the_daemon_has_been_up() {
    let started = Timestamp::now()
        .checked_sub(Duration::from_secs(2 * 3600 + 5 * 60 + 30))
        .unwrap();

    let health = health_with(Status::new(started)).await;

    assert!(health.contains("Uptime"), "{health}");
    assert!(health.contains(">2h 5m<"), "{health}");
}

#[tokio::test]
async fn health_shows_a_failing_daemon_with_its_streak_and_next_try() {
    let status = Status::new(at(1_000_000));
    status.failed(at(1_000_100), "connection refused");
    status.failed(at(1_000_160), "connection refused");
    status.retry_at(at(1_000_400));

    let health = health_with(status).await;

    assert!(health.contains("connection refused"), "{health}");
    assert!(health.contains("failed 2 in a row since"), "{health}");
    assert!(health.contains("next try"), "{health}");
    assert!(health.contains("alert-error"), "{health}");
    assert!(!health.contains("recovered"), "{health}");
}

#[tokio::test]
async fn health_shows_a_failure_without_a_retry_time_as_the_bare_error() {
    let status = Status::new(at(1_000_000));
    status.failed(at(1_000_100), "connection refused");

    let health = health_with(status).await;

    assert!(health.contains("connection refused"), "{health}");
    assert!(!health.contains("in a row"), "{health}");
    assert!(health.contains("alert-error"), "{health}");
}

#[tokio::test]
async fn health_shows_a_recovered_daemon_as_a_success() {
    let status = Status::new(at(1_000_000));
    status.failed(at(1_000_100), "connection refused");
    status.retry_at(at(1_000_400));
    status.pass_done(at(1_000_500), "all quiet");

    let health = health_with(status).await;

    assert!(health.contains("connection refused"), "{health}");
    assert!(health.contains("(recovered "), "{health}");
    assert!(health.contains("alert-success"), "{health}");
    assert!(!health.contains("in a row"), "{health}");
}

#[tokio::test]
async fn health_does_not_call_a_pass_without_an_error_a_recovery() {
    let status = Status::new(at(1_000_000));
    status.pass_done(at(1_000_100), "all quiet");

    let health = health_with(status).await;

    assert!(!health.contains("recovered"), "{health}");
    assert!(!health.contains("Last error"), "{health}");
}

mod passkeys {
    use url::Url;
    use webauthn_authenticator_rs::WebauthnAuthenticator;
    use webauthn_authenticator_rs::softpasskey::SoftPasskey;
    use webauthn_rs::prelude::Uuid;
    use webauthn_rs_proto::{
        AllowCredentials, CreationChallengeResponse, PublicKeyCredential, RequestChallengeResponse,
    };

    use super::super::auth::owner_id;
    use super::*;

    const CEREMONY_COOKIE: &str = "tamis_ceremony";

    fn origin() -> Url {
        Url::parse(ORIGIN).unwrap()
    }

    fn cookie_named(response: &Response, name: &str) -> Option<String> {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with(&format!("{name}=")))
            .and_then(|value| value.split(';').next())
            .map(str::to_owned)
    }

    async fn post_json(
        site: &Site,
        uri: &str,
        body: String,
        cookie: Option<&str>,
        signed_in: bool,
    ) -> Response {
        let mut request = Request::post(uri)
            .header(header::ORIGIN, ORIGIN)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        site.send(request.body(Body::from(body)).unwrap(), signed_in)
            .await
    }

    /// A ceremony the server has started: the options it sent and the cookie
    /// that ties the answer to it.
    struct Started<T> {
        options: T,
        cookie: String,
    }

    async fn start<T: serde::de::DeserializeOwned>(site: &Site, uri: &str) -> Started<T> {
        let response = post_json(site, uri, "{}".to_owned(), None, true).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let cookie = cookie_named(&response, CEREMONY_COOKIE).unwrap();
        let options = serde_json::from_str(&text(response).await).unwrap();
        Started { options, cookie }
    }

    /// The software authenticator can't hold resident keys, which tamis asks
    /// for, so the options it is given leave that out.
    fn without_resident_key(mut options: CreationChallengeResponse) -> CreationChallengeResponse {
        if let Some(selection) = options.public_key.authenticator_selection.as_mut() {
            selection.resident_key = None;
            selection.require_resident_key = false;
        }
        options
    }

    /// Enrols `soft`, returning the server's answer and the credential's id.
    async fn enrol(site: &Site, soft: &mut SoftPasskey, name: &str) -> (Response, Vec<u8>) {
        let started: Started<CreationChallengeResponse> = start(site, "/auth/register/begin").await;
        let credential = soft
            .do_registration(origin(), without_resident_key(started.options))
            .unwrap();
        let response = post_json(
            site,
            &format!("/auth/register/finish?name={name}"),
            serde_json::to_string(&credential).unwrap(),
            Some(&started.cookie),
            false,
        )
        .await;
        (response, credential.raw_id.clone())
    }

    /// What a resident-key authenticator would do: find the credential
    /// without being told which, and name the user in its answer.
    fn sign_in_as_owner(
        soft: &mut SoftPasskey,
        options: RequestChallengeResponse,
        credential_id: &[u8],
    ) -> PublicKeyCredential {
        let mut options = options;
        options.public_key.allow_credentials = vec![AllowCredentials {
            type_: "public-key".to_owned(),
            id: credential_id.to_vec(),
            transports: None,
        }];
        let mut assertion = soft.do_authentication(origin(), options).unwrap();
        assertion.response.user_handle = Some(owner_id().as_bytes().to_vec());
        assertion
    }

    #[tokio::test]
    async fn a_passkey_can_be_enrolled_and_then_signs_in() {
        let site = site();
        let mut soft = SoftPasskey::new(true);

        let (enrolled, credential_id) = enrol(&site, &mut soft, "laptop").await;

        assert_eq!(enrolled.status(), StatusCode::OK);
        assert!(cookie_named(&enrolled, SESSION_COOKIE).is_some());
        let after_enrolling = site.db.passkeys().unwrap();
        let laptop = after_enrolling.iter().find(|k| k.name == "laptop").unwrap();

        let started: Started<RequestChallengeResponse> = start(&site, "/auth/login/begin").await;
        let assertion = sign_in_as_owner(&mut soft, started.options, &credential_id);
        let signed_in = post_json(
            &site,
            "/auth/login/finish",
            serde_json::to_string(&assertion).unwrap(),
            Some(&started.cookie),
            false,
        )
        .await;

        assert_eq!(signed_in.status(), StatusCode::OK);
        let session = cookie_named(&signed_in, SESSION_COOKIE).unwrap();
        let page = site
            .send(
                Request::get("/")
                    .header(header::COOKIE, &session)
                    .body(Body::empty())
                    .unwrap(),
                false,
            )
            .await;
        assert_eq!(page.status(), StatusCode::OK, "the new session works");
        let after_login = site.db.passkeys().unwrap();
        let used = after_login.iter().find(|k| k.name == "laptop").unwrap();
        assert_ne!(
            used.credential, laptop.credential,
            "the signature counter was saved"
        );
    }

    async fn finish_login(site: &Site, assertion: &PublicKeyCredential, cookie: &str) -> Response {
        post_json(
            site,
            "/auth/login/finish",
            serde_json::to_string(assertion).unwrap(),
            Some(cookie),
            false,
        )
        .await
    }

    async fn enrolled_site() -> (Site, SoftPasskey, Vec<u8>) {
        let site = site();
        let mut soft = SoftPasskey::new(true);
        let (response, credential_id) = enrol(&site, &mut soft, "laptop").await;
        assert_eq!(response.status(), StatusCode::OK);
        (site, soft, credential_id)
    }

    #[tokio::test]
    async fn a_login_ceremony_can_be_finished_only_once() {
        let (site, mut soft, id) = enrolled_site().await;
        let started: Started<RequestChallengeResponse> = start(&site, "/auth/login/begin").await;
        let assertion = sign_in_as_owner(&mut soft, started.options, &id);

        let first = finish_login(&site, &assertion, &started.cookie).await;
        let replayed = finish_login(&site, &assertion, &started.cookie).await;

        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(replayed.status(), StatusCode::UNAUTHORIZED);
        assert!(cookie_named(&replayed, SESSION_COOKIE).is_none());
    }

    #[tokio::test]
    async fn an_answer_to_another_challenge_is_refused() {
        let (site, mut soft, id) = enrolled_site().await;
        let asked: Started<RequestChallengeResponse> = start(&site, "/auth/login/begin").await;
        let current: Started<RequestChallengeResponse> = start(&site, "/auth/login/begin").await;
        let assertion = sign_in_as_owner(&mut soft, asked.options, &id);

        let response = finish_login(&site, &assertion, &current.cookie).await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(cookie_named(&response, SESSION_COOKIE).is_none());
    }

    #[tokio::test]
    async fn a_passkey_that_was_revoked_cannot_sign_in() {
        let (site, mut soft, id) = enrolled_site().await;
        let laptop = site
            .db
            .passkeys()
            .unwrap()
            .into_iter()
            .find(|key| key.name == "laptop")
            .unwrap();
        site.db.remove_passkey(laptop.id).unwrap();
        let started: Started<RequestChallengeResponse> = start(&site, "/auth/login/begin").await;
        let assertion = sign_in_as_owner(&mut soft, started.options, &id);

        let response = finish_login(&site, &assertion, &started.cookie).await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(cookie_named(&response, SESSION_COOKIE).is_none());
    }

    #[tokio::test]
    async fn an_answer_for_another_user_is_refused() {
        let (site, mut soft, id) = enrolled_site().await;
        let started: Started<RequestChallengeResponse> = start(&site, "/auth/login/begin").await;
        let mut assertion = sign_in_as_owner(&mut soft, started.options, &id);
        assertion.response.user_handle = Some(Uuid::from_u128(1).as_bytes().to_vec());

        let response = finish_login(&site, &assertion, &started.cookie).await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(cookie_named(&response, SESSION_COOKIE).is_none());
    }

    #[tokio::test]
    async fn a_registration_answering_another_challenge_enrols_nothing() {
        let site = site();
        let mut soft = SoftPasskey::new(true);
        let asked: Started<CreationChallengeResponse> = start(&site, "/auth/register/begin").await;
        let current: Started<CreationChallengeResponse> =
            start(&site, "/auth/register/begin").await;
        let credential = soft
            .do_registration(origin(), without_resident_key(asked.options))
            .unwrap();

        let response = post_json(
            &site,
            "/auth/register/finish?name=laptop",
            serde_json::to_string(&credential).unwrap(),
            Some(&current.cookie),
            false,
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(cookie_named(&response, SESSION_COOKIE).is_none());
        assert!(
            site.db
                .passkeys()
                .unwrap()
                .iter()
                .all(|k| k.name != "laptop")
        );
    }
}
