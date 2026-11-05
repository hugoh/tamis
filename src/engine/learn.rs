use crate::classify::Label;
use crate::mail::{Mailbox, Message};
use crate::store::Decision;

use super::domain::{Action, Judgement, Reason, Verdict, keyword};
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

/// Whether the decision's only effect was flagging the message uncertain,
/// which learning replaces.
fn flagged_uncertain(decision: &Decision) -> bool {
    decision.action == Action::Keyword(keyword::UNCERTAIN.to_owned())
}

/// Says why a decision can't be learnt from. A correction is already applied
/// by nature, so it stays learnable to change one's mind.
fn refuse_learning(decision: &Decision, label: Option<Label>) -> Result<()> {
    let correction = decision.reason == Reason::Correction;
    if !decision.reason.is_own_judgement() && !correction {
        Err(Error::NotLearnable)
    } else if decision.applied && !correction && !flagged_uncertain(decision) {
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
    /// verdict. The message stays where it is, except one flagged uncertain:
    /// its flag is cleared, and it goes to Junk if learnt as spam. Returns a one-line account of
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

        if flagged_uncertain(&decision) && folder == decision.mailbox {
            self.settle_uncertain(&folder, &messages, learnt)
                .await
                .context(&context)?;
        }

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

    async fn settle_uncertain(
        &mut self,
        folder: &str,
        messages: &[Message],
        learnt: Label,
    ) -> Result<()> {
        let uids: Vec<u32> = messages.iter().map(|m| m.uid).collect();
        self.mail
            .remove_keyword(folder, &uids, keyword::UNCERTAIN)
            .await
            .context(format!("clear {}", keyword::UNCERTAIN))?;
        if learnt == Label::Spam {
            let junk = self.cfg.folders.junk.clone();
            self.mail
                .move_messages(folder, &uids, &junk)
                .await
                .context("move to junk")?;
        }
        Ok(())
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
