use crate::auth_pin::{AuthPin, AuthSource, resolve_auth_pin};
use crate::mail::Mailbox;

use super::Engine;

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
