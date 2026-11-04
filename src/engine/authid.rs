use crate::config::Config;
use crate::store::{self, Store};

use super::Engine;
use crate::mail::Mailbox;

/// The store key for the provider domain bootstrap learnt.
const AUTH_SETTING: &str = "auth_server_id";
/// The store key for the IMAP host the learnt domain belongs to.
const AUTH_HOST_SETTING: &str = "auth_server_id_host";

/// The `account.auth_server_id` value that turns pinning off: only the
/// topmost `Authentication-Results` header counts, whatever it names.
pub const NO_AUTH_SERVER_ID: &str = "none";

/// Where the authserv-id tamis pins to came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthSource {
    Configured,
    Learnt,
    Off,
    Unset,
    /// A domain was learnt for another IMAP host, so it is not used until
    /// bootstrap learns it again.
    Stale,
}

/// The authserv-id tamis pins `Authentication-Results` to. An empty ID means
/// pinning is off, which is safe: only the topmost header counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthPin {
    pub id: String,
    pub source: AuthSource,
    /// Set when the pin is stale: the host the domain was learnt for.
    pub learnt_for: String,
    /// Set when the pin is stale: the host the account uses now.
    pub host: String,
}

impl AuthPin {
    fn of(id: &str, source: AuthSource) -> Self {
        Self {
            id: id.to_owned(),
            source,
            learnt_for: String::new(),
            host: String::new(),
        }
    }
}

/// Works out the authserv-id to use: the configuration if it sets one, else
/// what bootstrap learnt, provided it was learnt for the account's current
/// host. A value learnt before hosts were recorded is used.
///
/// # Errors
/// Fails when the store can't be read.
pub fn resolve_auth_pin(store: &Store, cfg: &Config) -> std::result::Result<AuthPin, store::Error> {
    match cfg.account.auth_server_id.as_str() {
        NO_AUTH_SERVER_ID => return Ok(AuthPin::of("", AuthSource::Off)),
        "" => {}
        configured => return Ok(AuthPin::of(configured, AuthSource::Configured)),
    }

    let Some(learnt) = store.setting(AUTH_SETTING)? else {
        return Ok(AuthPin::of("", AuthSource::Unset));
    };
    if let Some(learnt_for) = store.setting(AUTH_HOST_SETTING)?
        && !learnt_for.eq_ignore_ascii_case(&cfg.account.host)
    {
        return Ok(AuthPin {
            id: String::new(),
            source: AuthSource::Stale,
            learnt_for,
            host: cfg.account.host.clone(),
        });
    }
    Ok(AuthPin::of(&learnt, AuthSource::Learnt))
}

impl<M: Mailbox> Engine<M> {
    /// The domain to pin `Authentication-Results` to, resolved once per
    /// engine.
    pub(super) fn auth_server_id(&mut self) -> String {
        if let Some(id) = &self.auth_id {
            return id.clone();
        }

        let pin = match resolve_auth_pin(&self.store, &self.cfg) {
            Ok(pin) => pin,
            Err(err) => {
                tracing::warn!(%err, "reading the learnt authserv-id failed; not pinning");
                AuthPin::of("", AuthSource::Unset)
            }
        };
        if pin.source == AuthSource::Stale {
            tracing::warn!(
                learnt_for = %pin.learnt_for,
                host = %pin.host,
                "the learnt authserv-id belongs to another host; not pinning. Run tamis bootstrap to learn it again"
            );
        }
        self.auth_id = Some(pin.id.clone());
        pin.id
    }
}
