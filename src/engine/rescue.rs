use crate::auth;
use crate::classify;
use crate::config::Mode;
use crate::mail::{Mailbox, Message};

use super::domain::{Action, Judgement, Reason, Summary, Verdict, keyword};
use super::{Context, Engine, Result};

impl<M: Mailbox> Engine<M> {
    /// Moves mail the provider filed to Junk back out when its sender is
    /// whitelisted and authenticated, or (if enabled) its score is very low.
    /// Reports whether it rescued the message.
    pub(super) async fn rescue(
        &mut self,
        message: &Message,
        summary: &mut Summary,
    ) -> Result<bool> {
        if !self.is_provider_filed(message)? {
            return Ok(false);
        }
        let Some(verdict) = self.judge_rescue(message)? else {
            return Ok(false);
        };

        summary.rescued += 1;
        let dest = self.cfg.folders.rescue_to().to_owned();
        let junk = self.cfg.folders.junk.clone();
        if self.cfg.policy.mode == Mode::Live {
            self.move_rescued(message, &verdict, &dest)
                .await
                .context("rescue")?;
        } else {
            self.record(&junk, message, &verdict, &Action::Move(dest), false)?;
        }
        Ok(true)
    }

    /// Requires authentication for whitelisted senders whatever
    /// `whitelist_requires_auth` says: impersonated contacts are what Junk
    /// catches.
    fn judge_rescue(&mut self, message: &Message) -> Result<Option<Judgement>> {
        if !message.from.is_empty() {
            let server_id = self.auth_server_id();
            if auth::verified(&message.raw, &server_id)
                && self.store.has_address(&message.from).context("rescue")?
            {
                return Ok(Some(Judgement {
                    verdict: Verdict::Good,
                    reason: Reason::Whitelisted,
                    score: 0.0,
                }));
            }
        }

        let threshold = self.cfg.policy.rescue_threshold;
        if threshold <= 0.0 {
            return Ok(None);
        }
        let (tokens, model) = self.features(&message.raw).context("rescue")?;
        let score = classify::score(&model, &tokens).score;
        Ok((score <= threshold).then_some(Judgement {
            verdict: Verdict::Good,
            reason: Reason::Bayes,
            score,
        }))
    }

    async fn move_rescued(
        &mut self,
        message: &Message,
        verdict: &Judgement,
        dest: &str,
    ) -> Result<()> {
        let junk = self.cfg.folders.junk.clone();

        self.mail
            .ensure(dest)
            .await
            .context(format!("create {dest}"))?;
        self.move_guarded(
            &junk,
            message,
            verdict,
            dest,
            &[keyword::RESCUED],
            &format!("move to {dest}"),
        )
        .await
    }
}
