//! Moves whose outcome was lost. A move is journalled before it is made, so
//! one cut off by a dropped connection or a timeout leaves a pending entry,
//! and the next pass settles it by looking for the message.

use crate::mail::{self, Mailbox, Message};

use super::domain::{Action, Judgement};
use super::{Context, Engine, Result};

impl<M: Mailbox> Engine<M> {
    /// Journals a move that is about to be made, as not yet applied. Without a
    /// Message-ID there is nothing to look for afterwards, so nothing is
    /// journalled yet and `settle_move` records it once the move has worked.
    pub(super) fn intend_move(
        &self,
        folder: &str,
        message: &Message,
        verdict: &Judgement,
        dest: &str,
        keyword: &str,
    ) -> Result<Option<i64>> {
        if message.message_id.is_empty() {
            return Ok(None);
        }
        let entry = Self::entry(
            folder,
            message,
            verdict,
            &Action::Move(dest.to_owned()),
            false,
        );
        let id = self
            .store
            .record_pending_move(&entry, dest, keyword)
            .context("journal move")?;
        Ok(Some(id))
    }

    /// Marks a move that worked as applied. If the store fails here the entry
    /// stays pending, and the next pass finds the message in `dest`.
    pub(super) fn settle_move(
        &self,
        intent: Option<i64>,
        folder: &str,
        message: &Message,
        verdict: &Judgement,
        dest: &str,
    ) -> Result<()> {
        let Some(id) = intent else {
            return self.record(
                folder,
                message,
                verdict,
                &Action::Move(dest.to_owned()),
                true,
            );
        };
        if let Err(err) = self.store.confirm_move(id) {
            tracing::warn!(%err, decision = id, "could not mark a move as applied; it is settled next pass");
        }
        Ok(())
    }

    /// Forgets a move the server refused. After a lost connection the move
    /// may have happened, so its entry is kept for the next pass.
    pub(super) fn withdraw_move(&self, intent: Option<i64>, err: &mail::Error) {
        let Some(id) = intent else {
            return;
        };
        if err.is_connection() {
            return;
        }
        if let Err(err) = self.store.drop_move(id) {
            tracing::warn!(%err, decision = id, "could not forget a move that did not happen; it is settled next pass");
        }
    }

    /// Settles the moves an earlier pass could not finish: one whose message
    /// is now in its destination is applied; any other did not happen, so its
    /// keyword is cleared and it is forgotten.
    pub(super) async fn resolve_pending_moves(&mut self) -> Result<()> {
        for pending in self.store.pending_moves().context("pending moves")? {
            match self.resolve_one(&pending).await {
                Err(err) if err.is_connection() => return Err(err),
                Err(err) => {
                    tracing::warn!(%err, decision = pending.decision_id, "could not settle a pending move");
                }
                Ok(()) => {}
            }
        }
        Ok(())
    }

    async fn resolve_one(&mut self, pending: &crate::store::PendingMove) -> Result<()> {
        let context = format!("settle move of {}", pending.message_id);
        let arrived = self
            .mail
            .find_message_id(&pending.dest, &pending.message_id)
            .await
            .context(&context)?;
        if !arrived.is_empty() {
            return self
                .store
                .confirm_move(pending.decision_id)
                .context(&context);
        }

        let left = self
            .mail
            .find_message_id(&pending.source, &pending.message_id)
            .await
            .context(&context)?;
        if !left.is_empty() {
            self.mail
                .remove_keyword(&pending.source, &left, &pending.keyword)
                .await
                .context(&context)?;
        }
        self.store.drop_move(pending.decision_id).context(&context)
    }
}
