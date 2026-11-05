use crate::classify::{self, Label};
use crate::mail::{Mailbox, Message};

use super::domain::{Action, Judgement, Reason, Summary, Verdict, keyword};
use super::keywords::{Mark, has_any_flag};
use super::{Context, Engine, Result, training_key};

impl<M: Mailbox> Engine<M> {
    /// Whether any policy wants the new arrivals in Junk.
    pub(super) fn wants_junk(&self) -> bool {
        let policy = &self.cfg.policy;
        policy.learn_from_junk || policy.rescue || policy.learn_provider_junk
    }

    /// Learns from what the user moved to Junk, then considers rescuing what
    /// the provider put there, and learns from what stays.
    pub(super) async fn handle_junk(
        &mut self,
        message: &Message,
        summary: &mut Summary,
    ) -> Result<()> {
        if self.cfg.policy.learn_from_junk && self.learn_from_junk(message, summary)? {
            return Ok(());
        }
        if self.cfg.policy.rescue && self.rescue(message, summary).await? {
            return Ok(());
        }
        if self.cfg.policy.learn_provider_junk {
            return self.learn_provider_junk(message, summary);
        }
        Ok(())
    }

    /// Treats a message tamis judged good or uncertain in a watched folder,
    /// now arriving in Junk, as one the user moved there: missed spam.
    /// Everything else in Junk (filed by the provider, by tamis, or through
    /// `TrainSpam`) is left alone.
    fn learn_from_junk(&self, message: &Message, summary: &mut Summary) -> Result<bool> {
        if !self.is_missed_spam(message)? {
            return Ok(false);
        }
        summary.corrected += 1;

        let tokens = self.tokens(&message.raw)?;
        let junk = self.cfg.folders.junk.clone();
        self.learn_spam(&junk, message, &tokens, Reason::Correction)?;
        Ok(true)
    }

    /// Trains mail the provider filed to Junk as spam when tamis isn't
    /// already sure it is: only those teach the model something. It trains the
    /// tokens and leaves the whitelist alone, since a forged message from a
    /// contact lands here too.
    fn learn_provider_junk(&self, message: &Message, summary: &mut Summary) -> Result<()> {
        if !self.is_provider_filed(message)? {
            return Ok(());
        }

        let (tokens, model) = self.features(&message.raw).context("learn provider junk")?;
        let score = classify::score(&model, &tokens).score;
        if score >= self.cfg.policy.spam_threshold {
            return Ok(());
        }

        self.store
            .train(&training_key(message), &tokens, Label::Spam)
            .context("learn provider junk")?;
        summary.trained += 1;

        let junk = self.cfg.folders.junk.clone();
        let judgement = Judgement {
            verdict: Verdict::Spam,
            reason: Reason::ProviderJunk,
            score,
        };
        self.record(&junk, message, &judgement, &Action::TrainSpam, true)
    }

    /// Whether the user moved this message to Junk after tamis let it through.
    fn is_missed_spam(&self, message: &Message) -> Result<bool> {
        if message.has_flag(keyword::FILED) {
            return Ok(false);
        }
        if self.is_rescue_moved_back(message)? {
            return Ok(true);
        }
        if self
            .store
            .trained(&training_key(message))
            .context("check training")?
            == Some(Label::Spam)
        {
            return Ok(false);
        }
        self.was_let_through(message)
    }

    /// Whether tamis's latest verdict on the message, in a watched folder,
    /// left it there.
    fn was_let_through(&self, message: &Message) -> Result<bool> {
        let decision = self
            .store
            .latest_decision(&message.message_id)
            .context("look up decision")?;
        Ok(decision.is_some_and(|d| {
            matches!(d.verdict.as_str(), "good" | "uncertain")
                && self.cfg.folders.watch.contains(&d.mailbox)
        }))
    }

    /// Whether this is mail tamis rescued from Junk that the user put back.
    fn is_rescue_moved_back(&self, message: &Message) -> Result<bool> {
        if !message.has_flag(keyword::RESCUED) {
            return Ok(false);
        }
        self.moved_by_tamis(message, Verdict::Good)
    }

    /// Whether a message in Junk is mail tamis never saw: the provider filed
    /// it. Anything tamis judged, trained or filed, and anything the user
    /// marked Junk, was put there deliberately.
    pub(super) fn is_provider_filed(&self, message: &Message) -> Result<bool> {
        if message.has_flag(keyword::FILED) || has_any_flag(&message.flags, Mark::Spam.keywords()) {
            return Ok(false);
        }
        if self
            .store
            .trained(&training_key(message))
            .context("check training")?
            .is_some()
        {
            return Ok(false);
        }
        let judged = self
            .store
            .latest_decision(&message.message_id)
            .context("look up decision")?;
        Ok(judged.is_none())
    }
}
