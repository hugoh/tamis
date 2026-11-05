//! Passkey enrolment, sign-in and sessions.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use jiff::Timestamp;
use tracing::warn;
use webauthn_rs::prelude::{Passkey, PublicKeyCredential, RegisterPublicKeyCredential, Uuid};

use webauthn_rs_proto::ResidentKeyRequirement;

use super::token::{b64, hash_token};
use super::{
    App, CEREMONY_COOKIE, CEREMONY_TTL, Ceremony, SESSION_COOKIE, SESSION_TTL, cookie, fail,
    set_cookie,
};
use crate::store;

pub(super) const OWNER_NAME: &str = "tamis";
const MAX_PASSKEY_NAME: usize = 64;
const DEFAULT_NAME: &str = "passkey";

/// The one user passkeys belong to, as the authenticator knows them.
fn owner_id() -> Uuid {
    Uuid::from_u128(u128::from_be_bytes(*b"tamis-owner-v1\0\0"))
}

/// The passkeys this build can use. Ones an older build enrolled are in
/// another format and have to be enrolled again.
fn load_passkeys(app: &App) -> Result<Vec<(String, Passkey)>, store::Error> {
    let mut usable = Vec::new();
    for key in app.db.passkeys()? {
        match serde_json::from_slice::<Passkey>(&key.credential) {
            Ok(passkey) => usable.push((key.credential_id, passkey)),
            Err(err) => {
                warn!(passkey = key.id, name = key.name, %err, "ignoring a passkey this build can't read: enrol it again");
            }
        }
    }
    Ok(usable)
}

fn passkey_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return DEFAULT_NAME.to_owned();
    }
    name.chars().take(MAX_PASSKEY_NAME).collect()
}

fn rejected(what: &str, err: &dyn std::fmt::Display) -> Response {
    warn!(%err, "{what}");
    (StatusCode::UNAUTHORIZED, "not accepted").into_response()
}

fn json_with_cookie(app: &App, ceremony: Ceremony, body: &impl serde::Serialize) -> Response {
    let id = match app.ceremonies.put_evicting(ceremony, CEREMONY_TTL) {
        Ok(id) => id,
        Err(err) => return fail("start ceremony", &err),
    };
    let mut response = Json(body).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(app, CEREMONY_COOKIE, &id, "/auth/", Some(CEREMONY_TTL)),
    );
    response
}

/// The ceremony this request started, which can't be used twice.
fn finish_ceremony(app: &App, headers: &HeaderMap) -> Option<Ceremony> {
    cookie(headers, CEREMONY_COOKIE).and_then(|id| app.ceremonies.take(&id))
}

fn restart() -> Response {
    (StatusCode::UNAUTHORIZED, "start again").into_response()
}

fn sign_in(app: &App) -> Response {
    let id = match app.sessions.put((), SESSION_TTL) {
        Ok(id) => id,
        Err(err) => return fail("start session", &err),
    };
    let mut response = Json(serde_json::json!({})).into_response();
    let headers = response.headers_mut();
    headers.append(
        header::SET_COOKIE,
        set_cookie(app, SESSION_COOKIE, &id, "/", Some(SESSION_TTL)),
    );
    headers.append(
        header::SET_COOKIE,
        set_cookie(app, CEREMONY_COOKIE, "", "/auth/", None),
    );
    response
}

pub(super) async fn register_begin(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<BTreeMap<String, String>>,
) -> Response {
    let keys = match load_passkeys(&app) {
        Ok(keys) => keys,
        Err(err) => return fail("begin registration", &err),
    };
    let exclude = keys.iter().map(|(_, key)| key.cred_id().clone()).collect();
    let (mut creation, state) = match app.webauthn.start_passkey_registration(
        owner_id(),
        OWNER_NAME,
        OWNER_NAME,
        Some(exclude),
    ) {
        Ok(started) => started,
        Err(err) => return fail("begin registration", &err),
    };

    // Last, so a failure above leaves the link usable.
    if !super::signed_in(&app, &headers) {
        let token = query.get("token").map_or("", String::as_str);
        match app
            .db
            .consume_enrollment(&hash_token(token), Timestamp::now())
        {
            Ok(true) => {}
            Ok(false) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    "this link has expired or was already used; run `tamis web enroll`",
                )
                    .into_response();
            }
            Err(err) => return fail("check enrollment token", &err),
        }
    }

    if let Some(selection) = creation.public_key.authenticator_selection.as_mut() {
        selection.resident_key = Some(ResidentKeyRequirement::Required);
        selection.require_resident_key = true;
    }
    json_with_cookie(&app, Ceremony::Register(Box::new(state)), &creation)
}

pub(super) async fn register_finish(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<BTreeMap<String, String>>,
    Json(credential): Json<RegisterPublicKeyCredential>,
) -> Response {
    let Some(Ceremony::Register(state)) = finish_ceremony(&app, &headers) else {
        return restart();
    };
    let passkey = match app
        .webauthn
        .finish_passkey_registration(&credential, &state)
    {
        Ok(passkey) => passkey,
        Err(err) => return rejected("finish registration", &err),
    };
    let blob = match serde_json::to_vec(&passkey) {
        Ok(blob) => blob,
        Err(err) => return fail("encode passkey", &err),
    };
    let name = passkey_name(query.get("name").map_or("", String::as_str));
    match app.db.add_passkey(&b64(passkey.cred_id()), &name, &blob) {
        Ok(()) => sign_in(&app),
        Err(store::Error::Exists(_)) => {
            (StatusCode::CONFLICT, "that passkey is already enrolled").into_response()
        }
        Err(err) => fail("save passkey", &err),
    }
}

pub(super) async fn login_begin(State(app): State<App>) -> Response {
    match app.webauthn.start_discoverable_authentication() {
        Ok((request, state)) => json_with_cookie(&app, Ceremony::Login(Box::new(state)), &request),
        Err(err) => fail("begin login", &err),
    }
}

pub(super) async fn login_finish(
    State(app): State<App>,
    headers: HeaderMap,
    Json(credential): Json<PublicKeyCredential>,
) -> Response {
    let Some(Ceremony::Login(state)) = finish_ceremony(&app, &headers) else {
        return restart();
    };
    let (user, credential_id) = match app
        .webauthn
        .identify_discoverable_authentication(&credential)
    {
        Ok(found) => found,
        Err(err) => return rejected("finish login", &err),
    };
    if user != owner_id() {
        return rejected("finish login", &"unknown user handle");
    }
    let keys = match load_passkeys(&app) {
        Ok(keys) => keys,
        Err(err) => return fail("finish login", &err),
    };
    let Some((stored_id, mut passkey)) = keys.into_iter().find(|(_, key)| {
        let id: &[u8] = key.cred_id().as_ref();
        id == credential_id
    }) else {
        return rejected("finish login", &"unknown passkey");
    };
    let result =
        match app
            .webauthn
            .finish_discoverable_authentication_v2(&credential, *state, &passkey)
        {
            Ok(result) => result,
            Err(err) => return rejected("finish login", &err),
        };
    if passkey.update_credential(&result).is_some() {
        match serde_json::to_vec(&passkey) {
            Ok(blob) => {
                if let Err(err) = app.db.update_passkey(&stored_id, &blob) {
                    return fail("record login", &err);
                }
            }
            Err(err) => return fail("encode passkey", &err),
        }
    }
    sign_in(&app)
}

pub(super) async fn logout(State(app): State<App>, headers: HeaderMap) -> Response {
    if let Some(id) = cookie(&headers, SESSION_COOKIE) {
        let _ = app.sessions.take(&id);
    }
    let mut response = Redirect::to("/auth/login").into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(&app, SESSION_COOKIE, "", "/", None),
    );
    response
}
