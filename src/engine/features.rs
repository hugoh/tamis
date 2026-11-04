use crate::classify;
use crate::mail::Mailbox;
use crate::store::Snapshot;

use super::{Context, Engine, Result};

impl<M: Mailbox> Engine<M> {
    /// Tokenizes a message, adds the recipient status, and returns a model to
    /// score the tokens with. The status depends on which addresses good mail
    /// has gone to, so it comes from the model; asking for its three possible
    /// tokens up front keeps this to one lookup.
    pub(super) fn features(&self, raw: &[u8]) -> Result<(Vec<String>, Snapshot)> {
        let tokens = classify::tokenize(raw);

        let mut wanted = tokens.clone();
        wanted.extend(classify::recipient_status_tokens().map(str::to_owned));
        let model = self.store.model(&wanted).context("features")?;

        let own = [self.cfg.account.user.clone()];
        Ok((
            classify::with_recipient_status(&tokens, &model, &own),
            model,
        ))
    }

    /// The score the model gives `raw` now, after it has been trained on it.
    pub(super) fn rescore(&self, raw: &[u8]) -> Result<f64> {
        let (tokens, model) = self.features(raw)?;
        Ok(classify::score(&model, &tokens).score)
    }
}
