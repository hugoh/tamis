#![allow(
    clippy::unwrap_used,
    reason = "a test that cannot read its fixtures should panic"
)]

use std::fs;
use std::path::Path;

use tamis::auth::{Brand, impersonated};

fn mail(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mail")
            .join(name),
    )
    .unwrap()
}

fn sendgrid() -> Vec<Brand> {
    vec![Brand {
        name: "sendgrid".into(),
        domains: vec!["sendgrid.net".into(), "sendgrid.com".into()],
    }]
}

fn from(header: &str) -> Vec<u8> {
    format!("From: {header}\r\nSubject: x\r\n\r\nbody\r\n").into_bytes()
}

#[test]
fn display_name_claiming_a_brand_from_another_domain_is_impersonation() {
    for file in [
        "phish-sendgrid-averiware.eml",
        "phish-sendgrid-naia.eml",
        "phish-sendgrid-leadseeker.eml",
        "phish-sendgrid-suiteconcierge.eml",
        "phish-sendgrid-bare-domain.eml",
    ] {
        let brands = sendgrid();
        assert_eq!(
            impersonated(&mail(file), &brands).map(|b| b.name.as_str()),
            Some("sendgrid"),
            "{file}"
        );
    }
}

#[test]
fn a_display_name_that_claims_nothing_is_not_flagged() {
    for file in ["phish-noreply-tellimer.eml", "phish-noreply-bombora.eml"] {
        assert!(impersonated(&mail(file), &sendgrid()).is_none(), "{file}");
    }
}

#[test]
fn the_brand_sending_as_itself_is_not_flagged() {
    for header in [
        "SendGrid <noreply@sendgrid.com>",
        "SendGrid Support <support@sendgrid.net>",
        "SENDGRID <alerts@mail.sendgrid.com>",
    ] {
        assert!(
            impersonated(&from(header), &sendgrid()).is_none(),
            "{header}"
        );
    }
}

#[test]
fn a_lookalike_domain_is_impersonation() {
    for header in [
        "SendGrid <noreply@notsendgrid.com>",
        "SendGrid <noreply@sendgrid.com.evil.example>",
    ] {
        assert!(
            impersonated(&from(header), &sendgrid()).is_some(),
            "{header}"
        );
    }
}

#[test]
fn a_message_with_no_from_or_no_brands_is_not_flagged() {
    assert!(impersonated(b"Subject: x\r\n\r\nbody\r\n", &sendgrid()).is_none());
    assert!(impersonated(&from("SendGrid <a@evil.example>"), &[]).is_none());
}
