#![allow(
    clippy::unwrap_used,
    reason = "a test that cannot read its fixtures should panic"
)]

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use tamis::auth::{self, Tally};

#[derive(Deserialize)]
struct Case {
    file: String,
    server_id: String,
    verified: bool,
}

#[derive(Deserialize)]
struct Tallied {
    name: String,
    parts: HashMap<String, usize>,
    total: usize,
    domain: String,
    ok: bool,
}

#[derive(Deserialize)]
struct Golden {
    auth: Vec<Case>,
    tallies: Vec<Tallied>,
}

fn mail(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mail")
            .join(name),
    )
    .unwrap()
}

fn golden() -> Golden {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden/classify_auth.json");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn verified_matches_the_go_package() {
    for case in golden().auth {
        let got = auth::verified(&mail(&case.file), &case.server_id);
        assert_eq!(
            got, case.verified,
            "{} with server id {:?}",
            case.file, case.server_id
        );
    }
}

#[test]
fn tally_matches_the_go_package() {
    for case in golden().tallies {
        let mut tally = Tally::default();
        for (file, count) in &case.parts {
            for _ in 0..*count {
                tally.add(&mail(file));
            }
        }

        assert_eq!(tally.total(), case.total, "{}", case.name);
        let inferred = tally.infer();
        assert_eq!(inferred.is_some(), case.ok, "{}", case.name);
        assert_eq!(inferred.unwrap_or_default(), case.domain, "{}", case.name);
    }
}
