//! The web interface: decisions, health, the whitelist and the rules, behind
//! passkeys.

mod auth;
pub mod cert;
mod listing;
mod pages;
#[cfg(test)]
mod tests;
mod token;
mod whitelist;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use cookie::{Cookie, SameSite};
use headers::{ETag, HeaderMapExt as _, IfNoneMatch};
use rust_embed::RustEmbed;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tower_http::compression::CompressionLayer;
use tracing::{debug, error, warn};
use url::Url;
use webauthn_rs::prelude::{
    DiscoverableAuthentication, PasskeyRegistration, Webauthn, WebauthnBuilder,
};

use crate::classify::Label;
use crate::config::Config;
use crate::live::{DebugSwitch, Ring, Status};
use crate::store::Store;

pub use token::{hash_token, new_token};

const SESSION_COOKIE: &str = "tamis_session";
const CEREMONY_COOKIE: &str = "tamis_ceremony";
const SESSION_TTL: Duration = Duration::from_hours(12);
const CEREMONY_TTL: Duration = Duration::from_mins(5);
const MAX_BODY: usize = 256 << 10;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

const CONTENT_SECURITY_POLICY: &str =
    "default-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'";

#[derive(RustEmbed)]
#[folder = "web/static/"]
struct Assets;

const REQUIRED_ASSETS: [&str; 2] = ["vendor/theme.css", "vendor/htmx.min.js"];

/// Whether this build carries the interface's CSS and JavaScript, which
/// `mise run web:vendor` fetches.
#[must_use]
pub fn has_assets() -> bool {
    missing_assets().is_empty()
}

fn missing_assets() -> Vec<&'static str> {
    REQUIRED_ASSETS
        .into_iter()
        .filter(|name| Assets::get(name).is_none())
        .collect()
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("this build has no web interface files ({0}): run mise run web:vendor, then rebuild")]
    NoAssets(String),
    #[error("set up passkeys: {0}")]
    Passkeys(String),
    #[error("{0}")]
    Tls(String),
    #[error("serve: {0}")]
    Serve(#[from] std::io::Error),
}

/// A change an authorised user asked for on a decision.
#[derive(Clone, Copy, Debug)]
pub enum Job {
    Undo(i64),
    Learn(i64, Option<Label>),
}

pub type JobFuture = Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;

/// Runs a [`Job`] against the mailbox, and says what it did.
pub type RunJobs = Arc<dyn Fn(Job) -> JobFuture + Send + Sync>;

#[derive(Default)]
pub struct Options {
    pub run: Option<RunJobs>,
    pub status: Option<Arc<Status>>,
    pub logs: Option<Arc<Ring>>,
    pub debug: Option<Arc<DebugSwitch>>,
    pub version: String,
}

enum Ceremony {
    Register(Box<PasskeyRegistration>),
    Login(Box<DiscoverableAuthentication>),
}

pub(crate) struct Shared {
    db: Arc<Store>,
    cfg: Arc<Config>,
    webauthn: Webauthn,
    origin: String,
    secure: bool,
    options: Options,
    action_lock: tokio::sync::Mutex<()>,
    sessions: token::Table<String>,
    ceremonies: token::Table<Ceremony>,
}

type App = Arc<Shared>;

/// The web interface, ready to serve.
pub struct Server {
    router: Router,
}

fn build(cfg: Arc<Config>, db: Arc<Store>, options: Options) -> Result<(Router, App), Error> {
    let origin = cfg.web.origin.trim_end_matches('/').to_owned();
    let url =
        Url::parse(&origin).map_err(|err| Error::Passkeys(format!("origin {origin:?}: {err}")))?;
    let rp_id = cfg.web.rpid();
    let webauthn = WebauthnBuilder::new(&rp_id, &url)
        .and_then(|builder| builder.rp_name(auth::OWNER_NAME).build())
        .map_err(|err| {
            let hint = if rp_id.parse::<std::net::IpAddr>().is_ok() {
                " (passkeys need a domain name such as localhost in web.origin, not an IP address)"
            } else {
                ""
            };
            Error::Passkeys(format!("{err}{hint}"))
        })?;

    let shared: App = Arc::new(Shared {
        db,
        cfg,
        webauthn,
        secure: origin.starts_with("https://"),
        origin,
        options,
        action_lock: tokio::sync::Mutex::new(()),
        sessions: token::Table::default(),
        ceremonies: token::Table::default(),
    });
    Ok((routes(Arc::clone(&shared)), shared))
}

impl Server {
    /// # Errors
    /// When the build lacks the interface files or the origin can't name a
    /// relying party.
    pub fn new(cfg: Arc<Config>, db: Arc<Store>, options: Options) -> Result<Self, Error> {
        let missing = missing_assets();
        if !missing.is_empty() {
            return Err(Error::NoAssets(missing.join(", ")));
        }
        Ok(Self {
            router: build(cfg, db, options)?.0,
        })
    }

    /// Serves on `listener` until `shutdown` is cancelled, over HTTPS when
    /// given a certificate and key.
    ///
    /// # Errors
    /// When the certificate can't be loaded or the listener fails.
    pub async fn serve(
        self,
        listener: tokio::net::TcpListener,
        tls: Option<(&std::path::Path, &std::path::Path)>,
        shutdown: CancellationToken,
    ) -> Result<(), Error> {
        let std_listener = listener.into_std()?;
        let handle = axum_server::Handle::new();
        let stopper = handle.clone();
        tokio::spawn(async move {
            shutdown.cancelled().await;
            stopper.graceful_shutdown(Some(SHUTDOWN_GRACE));
        });
        let service = self.router.into_make_service();
        match tls {
            Some((cert, key)) => {
                let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
                    .await
                    .map_err(|err| Error::Tls(format!("load certificate: {err}")))?;
                axum_server::from_tcp_rustls(std_listener, config)?
                    .handle(handle)
                    .serve(service)
                    .await?;
            }
            None => {
                axum_server::from_tcp(std_listener)?
                    .handle(handle)
                    .serve(service)
                    .await?;
            }
        }
        Ok(())
    }
}

fn routes(shared: App) -> Router {
    let signed_in = Router::new()
        .route("/", get(pages::decisions))
        .route("/health", get(pages::health))
        .route("/rules", get(pages::rules))
        .route("/rules/test", post(pages::rules_test))
        .route("/logs", get(pages::logs))
        .route("/logs/debug", post(pages::set_debug))
        .route("/whitelist", get(whitelist::page).post(whitelist::add))
        .route("/whitelist/remove", post(whitelist::remove))
        .route("/whitelist/remove/confirm", post(whitelist::confirm))
        .route("/decisions/{id}/undo", post(pages::undo))
        .route("/decisions/{id}/learn", post(pages::learn))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&shared),
            require_session,
        ));

    Router::new()
        .route("/static/{*path}", get(static_file))
        .route("/auth/login", get(pages::login))
        .route("/auth/enroll", get(pages::enroll))
        .route("/auth/register/begin", post(auth::register_begin))
        .route("/auth/register/finish", post(auth::register_finish))
        .route("/auth/login/begin", post(auth::login_begin))
        .route("/auth/login/finish", post(auth::login_finish))
        .route("/auth/logout", post(auth::logout))
        .merge(signed_in)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(Arc::clone(&shared), guard))
        .layer(CompressionLayer::new())
        .with_state(shared)
}

fn header_value(value: &'static str) -> HeaderValue {
    HeaderValue::from_static(value)
}

/// Sets the security headers, refuses changes that don't come from the page's
/// own origin, and logs each request at debug.
async fn guard(State(app): State<App>, request: Request, next: Next) -> Response {
    let (method, path) = (request.method().clone(), request.uri().path().to_owned());
    let started = std::time::Instant::now();

    let mut response = if method != Method::GET
        && method != Method::HEAD
        && !from_own_origin(&app, request.headers())
    {
        (StatusCode::FORBIDDEN, "forbidden").into_response()
    } else {
        next.run(request).await
    };

    let headers = response.headers_mut();
    for (name, value) in [
        (header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
    ] {
        headers.insert(name, header_value(value));
    }
    if app.secure {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            header_value("max-age=31536000"),
        );
    }
    if !path.starts_with("/static/") {
        headers.insert(header::CACHE_CONTROL, header_value("no-store"));
    }
    debug!(%method, %path, status = response.status().as_u16(), duration = ?started.elapsed(), "{}", crate::live::WEB_REQUEST);
    response
}

fn from_own_origin(app: &Shared, headers: &HeaderMap) -> bool {
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        Some(origin) if origin == app.origin => true,
        Some("null") => headers
            .get(HeaderName::from_static("sec-fetch-site"))
            .is_some_and(|site| site == "same-origin"),
        _ => false,
    }
}

async fn require_session(State(app): State<App>, request: Request, next: Next) -> Response {
    if signed_in(&app, request.headers()) {
        return next.run(request).await;
    }
    if request.method() == Method::GET {
        Redirect::to("/auth/login").into_response()
    } else {
        (StatusCode::UNAUTHORIZED, "sign in first").into_response()
    }
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(Cookie::split_parse)
        .filter_map(Result::ok)
        .find(|pair| pair.name() == name)
        .map(|pair| pair.value().to_owned())
}

fn signed_in(app: &Shared, headers: &HeaderMap) -> bool {
    cookie(headers, SESSION_COOKIE)
        .and_then(|id| app.sessions.get(&id))
        .is_some_and(|credential_id| match app.db.has_passkey(&credential_id) {
            Ok(found) => found,
            Err(err) => {
                warn!(%err, "check the session's passkey: treating the request as signed out");
                false
            }
        })
}

/// A `Set-Cookie` value. `Secure` follows the configured origin: plain HTTP is
/// for loopback.
fn set_cookie(
    app: &Shared,
    name: &str,
    value: &str,
    path: &str,
    ttl: Option<Duration>,
) -> HeaderValue {
    let max_age = ttl.map_or(time::Duration::ZERO, |ttl| {
        time::Duration::try_from(ttl).unwrap_or(time::Duration::MAX)
    });
    let cookie = Cookie::build((name, value))
        .path(path)
        .max_age(max_age)
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(app.secure);
    HeaderValue::from_str(&cookie.to_string()).unwrap_or_else(|err| {
        warn!(%err, cookie = name, "cookie value is not a valid header: sending none");
        HeaderValue::from_static("")
    })
}

fn content_type(path: &str) -> &'static str {
    mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream")
}

async fn static_file(
    axum::extract::Path(path): axum::extract::Path<String>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
    headers: HeaderMap,
) -> Response {
    let Some(file) = Assets::get(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let etag = format!("\"{}\"", hex::encode(file.metadata.sha256_hash()));
    let unchanged = etag.parse::<ETag>().is_ok_and(|current| {
        headers
            .typed_get::<IfNoneMatch>()
            .is_some_and(|sent| !sent.precondition_passes(&current))
    });
    let versioned = query
        .as_deref()
        .and_then(|query| query.split('&').find_map(|pair| pair.strip_prefix("v=")))
        .is_some_and(|version| !version.is_empty());
    let cache_control = if versioned {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let builder = Response::builder()
        .header(header::ETAG, &etag)
        .header(header::CACHE_CONTROL, cache_control);
    let response = if unchanged {
        builder.status(StatusCode::NOT_MODIFIED).body(Body::empty())
    } else {
        builder
            .header(header::CONTENT_TYPE, content_type(&path))
            .body(Body::from(file.data.into_owned()))
    };
    response.unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Logs an unexpected failure and says only that it happened.
pub(crate) fn fail(what: &str, err: &dyn std::fmt::Display) -> Response {
    error!(%err, "{what}");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
}
