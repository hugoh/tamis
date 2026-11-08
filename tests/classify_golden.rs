#![allow(
    clippy::unwrap_used,
    reason = "a test that cannot read its fixtures should panic"
)]

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use tamis::classify::{self, Label, Model};

#[derive(Deserialize)]
struct Clue {
    token: String,
    prob: f64,
}

#[derive(Deserialize)]
struct Case {
    file: String,
    tokens: Vec<String>,
    with_status: Vec<String>,
    score: f64,
    clues: Vec<Clue>,
}

#[derive(Deserialize)]
struct Golden {
    own: Vec<String>,
    train_good: Vec<String>,
    train_spam: Vec<String>,
    cases: Vec<Case>,
}

#[derive(Default)]
struct Memory {
    counts: HashMap<String, (u64, u64)>,
    spam: u64,
    good: u64,
}

impl Model for Memory {
    fn counts(&self, token: &str) -> (u64, u64) {
        self.counts.get(token).copied().unwrap_or_default()
    }

    fn totals(&self) -> (u64, u64) {
        (self.spam, self.good)
    }
}

impl Memory {
    fn train(&mut self, tokens: &[String], label: Label) {
        for token in tokens {
            let entry = self.counts.entry(token.clone()).or_default();
            match label {
                Label::Spam => entry.0 += 1,
                Label::Good => entry.1 += 1,
            }
        }
        match label {
            Label::Spam => self.spam += 1,
            Label::Good => self.good += 1,
        }
    }
}

fn mail(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mail")
        .join(name);
    fs::read(path).unwrap()
}

#[test]
fn matches_the_recorded_results() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden/classify_auth.json");
    let golden: Golden = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();

    let mut model = Memory::default();
    for (label, files) in [
        (Label::Good, &golden.train_good),
        (Label::Spam, &golden.train_spam),
    ] {
        for file in files {
            let tokens = classify::with_recipient_status(
                &classify::tokenize(&mail(file)),
                &model,
                &golden.own,
            );
            model.train(&tokens, label);
        }
    }

    for case in &golden.cases {
        let tokens = classify::tokenize(&mail(&case.file));
        assert_eq!(tokens, case.tokens, "tokens of {}", case.file);

        let with_status = classify::with_recipient_status(&tokens, &model, &golden.own);
        assert_eq!(
            with_status, case.with_status,
            "status tokens of {}",
            case.file
        );

        let result = classify::score(&model, &with_status);
        assert!(
            (result.score - case.score).abs() < 1e-9,
            "score of {}: {} vs {}",
            case.file,
            result.score,
            case.score
        );
        assert_eq!(
            result.clues.len(),
            case.clues.len(),
            "clue count of {}",
            case.file
        );
        for (got, want) in result.clues.iter().zip(&case.clues) {
            assert_eq!(got.token, want.token, "clue order of {}", case.file);
            assert!(
                (got.prob - want.prob).abs() < 1e-9,
                "clue {} of {}",
                want.token,
                case.file
            );
        }
    }
}
