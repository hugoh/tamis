//! Sample data for looking at the web interface, and a live server over it.
//!
//! `mock_decisions_page` (in the parent module) freezes the hand-written rows
//! into a static page. `serve_mock` below serves the same rows, plus enough
//! more to exercise paging, from the real router, so every interaction works
//! against throwaway data: `mise run web:serve`.

use std::sync::OnceLock;

use axum::middleware::{self, Next};

use super::*;
use crate::classify::Label;
use crate::live::{DebugSwitch, Entry, Ring};

/// The hand-written rows: one of each state the decisions page can show.
#[allow(clippy::too_many_lines, reason = "a list of sample rows")]
pub(super) fn seed_decisions(site: &Site) {
    record_as(
        site,
        "Your receipt from Acme",
        ("good", 0.03),
        "rule",
        "move:Receipts",
        true,
    );
    record_as(
        site,
        "Weekly digest",
        ("good", 0.08),
        "rule",
        "move:Newsletters",
        true,
    );
    record_as(site, "Meeting notes", ("good", 0.02), "bayes", "none", true);
    record_as(
        site,
        "Newsletter",
        ("good", 0.0),
        "whitelisted",
        "none",
        true,
    );
    record_as(
        site,
        "Is this you?",
        ("uncertain", 0.52),
        "bayes",
        "none",
        true,
    );
    record_as(
        site,
        "You won a prize!!!",
        ("spam", 0.99),
        "bayes",
        "move:Junk",
        true,
    );
    let rescued = record_as(
        site,
        "Lunch on Friday?",
        ("spam", 0.91),
        "bayes",
        "move:Junk",
        true,
    );
    site.db.mark_undone(rescued).unwrap();
    // A real undo also journals an `undo` entry for the message, which is what
    // dims the original row.
    record_as(
        site,
        "Lunch on Friday?",
        ("good", 0.0),
        "undo",
        "move:INBOX",
        true,
    );
    // Same subject, so the same Message-ID: the later correction dims the
    // original row in the page.
    record_as(
        site,
        "Invoice #4821",
        ("spam", 0.88),
        "bayes",
        "move:Junk",
        true,
    );
    record_as(
        site,
        "Invoice #4821",
        ("good", 0.0),
        "correction",
        "move:INBOX",
        true,
    );

    record_as(
        site,
        "Cheap watches",
        ("spam", 0.97),
        "bayes",
        "move:Junk",
        false,
    );
}

/// Enough extra rows to need a second page, and a long whitelist to match.
fn seed_bulk(site: &Site, rows: usize) {
    const VERDICTS: [(&str, f64, &str, &str); 4] = [
        ("good", 0.02, "bayes", "none"),
        ("spam", 0.97, "bayes", "move:Junk"),
        ("good", 0.0, "whitelisted", "none"),
        ("uncertain", 0.5, "bayes", "none"),
    ];
    for n in 0..rows {
        let (verdict, score, reason, action) = VERDICTS[n % VERDICTS.len()];
        record_as(
            site,
            &format!("Sample message {}", n + 1),
            (verdict, score),
            reason,
            action,
            n % 5 != 0,
        );
    }
    for n in 0..rows {
        site.db
            .add_address(
                &format!("person{n:03}@example.org"),
                if n % 3 == 0 { "manual" } else { "sent" },
            )
            .unwrap();
    }
}

const SAMPLE_RULES: &str = "require [\"fileinto\"];\n\
    # Sample rules for the mock interface.\n\
    if header :contains \"list-id\" \"announce.example.org\" {\n    fileinto \"Newsletters\";\n}\n\
    elsif address :domain :is \"from\" \"acme.example.com\" {\n    fileinto \"Receipts\";\n}\n";

/// Journals what the mailbox would have done, as a real action does.
fn journal(db: &Store, original: &Decision, verdict: &str, reason: &str, action: &str) {
    db.record_decision(&Decision {
        at: Timestamp::now(),
        verdict: verdict.parse().unwrap(),
        reason: reason.parse().unwrap(),
        action: action.parse().unwrap(),
        applied: true,
        score: 0.0,
        ..original.clone()
    })
    .unwrap();
}

/// Stands in for the mailbox: Undo and Spam/Good change the sample store so the
/// list reacts, or every action fails when `fail` is set.
fn fake_run(db: Arc<OnceLock<Arc<Store>>>, fail: bool) -> RunJobs {
    Arc::new(move |job| {
        let db = db.get().map(Arc::clone);
        Box::pin(async move {
            if fail {
                return Err("the mailbox moved on".to_owned());
            }
            let db = db.ok_or("the sample store is not ready")?;
            match job {
                Job::Undo(id) => {
                    let original = db.decision(id).map_err(|err| err.to_string())?;
                    db.mark_undone(id).map_err(|err| err.to_string())?;
                    journal(&db, &original, "good", "undo", "move:INBOX");
                    Ok("moved back to INBOX".to_owned())
                }
                Job::Learn(id, label) => {
                    let original = db.decision(id).map_err(|err| err.to_string())?;
                    let (verdict, action, said) = match label {
                        Some(Label::Spam) => {
                            ("spam", "move:Junk", "learned as spam, moved to Junk")
                        }
                        _ => ("good", "move:INBOX", "learned as good, moved to INBOX"),
                    };
                    journal(&db, &original, verdict, "correction", action);
                    Ok(said.to_owned())
                }
            }
        })
    })
}

fn sample_logs() -> Arc<Ring> {
    let ring = Arc::new(Ring::new(1000));
    ring.start();
    for (level, text) in [
        (tracing::Level::INFO, "pass started: INBOX, Junk"),
        (
            tracing::Level::DEBUG,
            "INBOX: 3 new messages since uid 4821",
        ),
        (
            tracing::Level::INFO,
            "moved \"You won a prize!!!\" to Junk (score 0.99)",
        ),
        (
            tracing::Level::WARN,
            "Junk: slow response from imap.example.org (2.4s)",
        ),
        (tracing::Level::ERROR, "Sent: connection reset, will retry"),
        (tracing::Level::INFO, "pass done: 3 moved, 41 kept"),
    ] {
        ring.add(Entry {
            at: Timestamp::now(),
            level,
            text: text.to_owned(),
        });
    }
    ring
}

/// Serves the real router over sample data until interrupted. Settings, all
/// optional: `TAMIS_MOCK_PORT` (8484), `TAMIS_MOCK_ROWS` (150, more than one
/// page), `TAMIS_MOCK_FAIL` (any value: every action and the daemon fail),
/// `TAMIS_MOCK_LATENCY` (350: the slowest a page or action may take to answer,
/// in milliseconds; 0 answers at once).
#[tokio::test]
#[ignore = "serves until interrupted; run by hand: mise run web:serve"]
async fn serve_mock() {
    let var = |name: &str| std::env::var(name).ok();
    let port: u16 = var("TAMIS_MOCK_PORT").map_or(8484, |v| v.parse().expect("TAMIS_MOCK_PORT"));
    let rows: usize = var("TAMIS_MOCK_ROWS").map_or(150, |v| v.parse().expect("TAMIS_MOCK_ROWS"));
    let fail = var("TAMIS_MOCK_FAIL").is_some();
    let max_latency: u64 =
        var("TAMIS_MOCK_LATENCY").map_or(350, |v| v.parse().expect("TAMIS_MOCK_LATENCY"));
    let origin = format!("http://localhost:{port}");

    let db = Arc::new(OnceLock::new());
    let status = Arc::new(Status::new(
        Timestamp::now()
            .checked_sub(Duration::from_mins(200))
            .unwrap(),
    ));
    status.pass_done(Timestamp::now(), "3 moved, 41 kept");
    if fail {
        status.failed(Timestamp::now(), "IMAP login rejected: check the password");
    }
    let ring = sample_logs();
    let debug = Arc::new(DebugSwitch::new(
        false,
        Duration::from_secs(600),
        Arc::clone(&ring),
        |_| {},
    ));
    let site = site_at(
        &origin,
        Options {
            run: Some(fake_run(Arc::clone(&db), fail)),
            status: Some(status),
            logs: Some(ring),
            debug: Some(debug),
            ..Options::default()
        },
        SAMPLE_RULES,
    );
    db.set(Arc::clone(&site.db)).ok();
    seed_decisions(&site);
    seed_bulk(&site, rows);

    // The test helper's session lasts a minute: mint one for the whole sitting
    // and put it on every request but the sign-in pages (`/auth/...`), which stay
    // viewable; there is no passkey to sign in with.
    let session = site
        .app
        .sessions
        .put("session-key".to_owned(), Duration::from_hours(12))
        .unwrap();
    let cookie = HeaderValue::from_str(&format!("{SESSION_COOKIE}={session}")).unwrap();
    let router = site.router.clone().layer(middleware::from_fn(
        move |request: Request<Body>, next: Next| async move {
            if max_latency > 0 && !request.uri().path().starts_with("/static/") {
                let delay = fastrand::u64(max_latency / 8..=max_latency);
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            next.run(request).await
        },
    ));
    let router = router.layer(middleware::from_fn(
        move |mut request: Request<Body>, next: Next| {
            let cookie = cookie.clone();
            async move {
                if !request.uri().path().starts_with("/auth/") {
                    request.headers_mut().insert(header::COOKIE, cookie);
                }
                next.run(request).await
            }
        },
    ));

    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    listener.set_nonblocking(true).unwrap();
    println!("tamis mock web interface: {origin}/  (Ctrl-C to stop)");
    tokio::select! {
        served = axum_server::from_tcp(listener)
            .unwrap()
            .serve(router.into_make_service()) => served.unwrap(),
        _ = tokio::signal::ctrl_c() => {}
    }
}
