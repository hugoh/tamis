use crate::classify::Label;
use crate::mail::{Mailbox, Message};
use crate::store::Decision;

use super::domain::{Action, Judgement, Reason, Verdict};
use super::{Context, Engine, Error, Result};

/// The label to learn for `decision`, given the one asked for, if any.
fn learnt_label(decision: &Decision, label: Option<Label>) -> Result<Label> {
    refuse_learning(decision, label)?;
    match (label, decision.verdict.as_str()) {
        (Some(label), _) => Ok(label),
        (None, "spam") => Ok(Label::Good),
        (None, "good") => Ok(Label::Spam),
        (None, _) => Err(Error::NeedsLabel),
    }
}

/// Says why a decision can't be learnt from. A correction is already applied
/// by nature, so it stays learnable to change one's mind.
fn refuse_learning(decision: &Decision, label: Option<Label>) -> Result<()> {
    let correction = decision.reason == Reason::Correction.as_str();
    if !matches!(decision.reason.as_str(), "bayes" | "whitelisted") && !correction {
        Err(Error::NotLearnable)
    } else if decision.applied && !correction {
        Err(Error::AlreadyApplied)
    } else if label.is_none() && decision.corrected_by > 0 {
        Err(Error::AlreadyCorrected(decision.corrected_by))
    } else {
        Ok(())
    }
}

impl<M: Mailbox> Engine<M> {
    /// Teaches tamis the right answer for a decision it only recorded, such
    /// as one made in shadow mode. With no label it learns the opposite of the
    /// verdict. The message stays where it is. Returns a one-line account of
    /// what it did.
    ///
    /// # Errors
    /// Fails when the decision can't be learnt from or the message can't be
    /// found.
    pub async fn learn(&mut self, decision_id: i64, label: Option<Label>) -> Result<String> {
        let context = format!("learn decision {decision_id}");
        let decision = self.store.decision(decision_id).context("learn")?;
        let learnt = learnt_label(&decision, label).context(&context)?;

        let (folder, messages) = self.locate_for_learn(&decision).await.context(&context)?;
        self.relearn(&messages[0], learnt).context(&context)?;

        let score = self.rescore(&messages[0].raw).context(&context)?;
        let (verdict, action, account) = match learnt {
            Label::Spam => (
                Verdict::Spam,
                Action::TrainSpam,
                "learnt as spam, sender removed from whitelist",
            ),
            Label::Good => (
                Verdict::Good,
                Action::TrainGood,
                "learnt as good, sender whitelisted",
            ),
        };
        let judgement = Judgement {
            verdict,
            reason: Reason::Correction,
            score,
        };
        self.record(&folder, &messages[0], &judgement, &action, true)
            .context(&context)?;
        Ok(account.to_owned())
    }

    /// Finds the message where it was judged or, if the user has since sent it
    /// to Junk, there.
    async fn locate_for_learn(&mut self, decision: &Decision) -> Result<(String, Vec<Message>)> {
        let mut last = None;
        for folder in [decision.mailbox.clone(), self.cfg.folders.junk.clone()] {
            match self.locate(&folder, &decision.message_id).await {
                Err(err) if err.is_message_gone() => last = Some(err),
                other => return other.map(|messages| (folder, messages)),
            }
        }
        Err(last.unwrap_or(Error::NotLearnable))
    }
}
