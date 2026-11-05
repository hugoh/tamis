use crate::classify::Label;
use crate::mail::{Mailbox, Message};
use crate::store::{self, Decision};

use super::domain::{Action, Judgement, Reason, Verdict, keyword};
use super::{Context, Engine, Error, Result, training_key};

/// How a decision is reversed.
enum Undoer {
    Move,
    Uncertain,
}

/// Says how to reverse a decision, or why it can't be.
fn undoer_for(decision: &Decision) -> Result<Undoer> {
    if decision.undone {
        return Err(store::Error::AlreadyUndone(decision.id).into());
    }
    if !decision.applied {
        return Err(Error::NothingToUndo);
    }
    if !decision.reason.is_own_judgement() {
        return Err(Error::NotUndoable);
    }
    if matches!(decision.action, Action::Move(_)) {
        Ok(Undoer::Move)
    } else if decision.action == Action::Keyword(keyword::UNCERTAIN.to_owned()) {
        Ok(Undoer::Uncertain)
    } else {
        Err(Error::NotUndoable)
    }
}

/// Whether the decision can still be undone, and is not a flag a later
/// correction has already settled.
#[must_use]
pub fn can_undo(decision: &Decision) -> bool {
    undoer_for(decision).is_ok()
        && !(super::learn::flagged_uncertain(decision) && decision.corrected_by > 0)
}

/// What undoing a move teaches: the keyword tamis set, and the label and
/// verdict the message really deserved.
struct Reversal {
    keyword: &'static str,
    label: Label,
    verdict: Verdict,
}

impl<M: Mailbox> Engine<M> {
    /// Reverses a decision tamis applied:
    /// - mail moved to Junk is learnt as good, its sender whitelisted, and
    ///   moved back to the folder it came from;
    /// - mail rescued from Junk is learnt as spam and moved back;
    /// - mail flagged uncertain has the flag cleared.
    ///
    /// Returns a one-line account of what it did. A failed undo leaves the
    /// decision as it was, so it can be retried.
    ///
    /// # Errors
    /// Fails when the decision can't be undone or the mailbox can't be
    /// changed.
    pub async fn undo(&mut self, decision_id: i64) -> Result<String> {
        let decision = self.store.decision(decision_id).context("undo")?;
        let context = format!("undo decision {decision_id}");

        let summary = match undoer_for(&decision).context(&context)? {
            Undoer::Move => self.undo_move(&decision).await,
            Undoer::Uncertain => self.undo_uncertain(&decision).await,
        }
        .context(&context)?;

        self.store.mark_undone(decision_id).context(&context)?;
        Ok(summary)
    }

    /// Brings a message back to where it was before tamis moved it, teaching
    /// tamis it was wrong: filed spam is learnt as good and its sender
    /// whitelisted; a rescue is learnt as spam and its sender dropped from the
    /// whitelist.
    async fn undo_move(&mut self, decision: &Decision) -> Result<String> {
        let Action::Move(moved_to) = &decision.action else {
            return Err(Error::NotUndoable);
        };
        let reversal = if decision.verdict == Verdict::Good {
            Reversal {
                keyword: keyword::RESCUED,
                label: Label::Spam,
                verdict: Verdict::Spam,
            }
        } else {
            Reversal {
                keyword: keyword::FILED,
                label: Label::Good,
                verdict: Verdict::Good,
            }
        };

        let messages = self.locate(moved_to, &decision.message_id).await?;
        let uids: Vec<u32> = messages.iter().map(|m| m.uid).collect();
        for message in &messages {
            self.relearn(message, reversal.label)?;
        }

        self.mail
            .remove_keyword(moved_to, &uids, reversal.keyword)
            .await
            .context(format!("clear {}", reversal.keyword))?;
        self.mail
            .add_keyword(moved_to, &uids, keyword::TRAINED)
            .await
            .context(format!("flag {}", keyword::TRAINED))?;
        self.mail
            .move_messages(moved_to, &uids, &decision.mailbox)
            .await
            .context("move back")?;

        let judgement = Judgement {
            verdict: reversal.verdict,
            reason: Reason::Undo,
            score: 0.0,
        };
        self.record(
            &decision.mailbox,
            &messages[0],
            &judgement,
            &Action::Move(decision.mailbox.clone()),
            true,
        )?;

        let learnt = if reversal.label == Label::Spam {
            "learnt as spam, sender removed from whitelist"
        } else {
            "learnt as good, sender whitelisted"
        };
        Ok(format!("moved back to {}, {learnt}", decision.mailbox))
    }

    async fn undo_uncertain(&mut self, decision: &Decision) -> Result<String> {
        let messages = self.locate(&decision.mailbox, &decision.message_id).await?;
        let uids: Vec<u32> = messages.iter().map(|m| m.uid).collect();

        self.mail
            .remove_keyword(&decision.mailbox, &uids, keyword::UNCERTAIN)
            .await
            .context(format!("clear {}", keyword::UNCERTAIN))?;

        let judgement = Judgement {
            verdict: Verdict::Uncertain,
            reason: Reason::Undo,
            score: 0.0,
        };
        self.record(
            &decision.mailbox,
            &messages[0],
            &judgement,
            &Action::Unflag(keyword::UNCERTAIN.to_owned()),
            true,
        )?;
        Ok(format!("cleared {}", keyword::UNCERTAIN))
    }

    /// Trains `message` as `label` and updates the whitelist to match.
    pub(super) fn relearn(&self, message: &Message, label: Label) -> Result<()> {
        let context = format!("learn {}", label.as_str());
        let tokens = self.tokens(&message.raw).context(&context)?;
        self.store
            .train(&training_key(message), &tokens, label)
            .context(&context)?;

        if message.from.is_empty() {
            return Ok(());
        }
        match label {
            Label::Good => self.store.add_address(&message.from, Reason::Undo.as_str()),
            Label::Spam => self.store.remove_address(&message.from),
        }
        .context("update whitelist")
    }
}
