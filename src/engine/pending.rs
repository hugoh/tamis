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
        keywords: &[&str],
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
            .record_pending_move(&entry, dest, keywords)
            .context("journal move")?;
        Ok(Some(id))
    }

    /// Moves `message` from `source` to `dest` so that a failure at any step
    /// leaves nothing half done. `marks` (keywords and flags) are set on the
    /// message first, and the move is journalled before it is made. If the
    /// server refuses a step, the marks are cleared and the journal forgets
    /// the move; if the connection is lost, the journal keeps the move and
    /// the next pass settles it (see `resolve_pending_moves`).
    ///
    /// Every move that sets marks beforehand goes through here.
    pub(super) async fn move_guarded(
        &mut self,
        source: &str,
        message: &Message,
        verdict: &Judgement,
        dest: &str,
        marks: &[&str],
        what: &str,
    ) -> Result<()> {
        let uids = [message.uid];
        self.add_marks(source, &uids, marks).await.context(what)?;
        let intent = self.intend_move(source, message, verdict, dest, marks)?;
        if let Err(err) = self.mail.move_messages(source, &uids, dest).await {
            self.clear_marks(source, &uids, marks).await;
            self.withdraw_move(intent, &err);
            return Err(err).context(what);
        }
        self.settle_move(intent, source, message, verdict, dest)
    }

    /// Sets `marks`; if one fails, the ones already set are cleared again.
    pub(super) async fn add_marks(
        &mut self,
        folder: &str,
        uids: &[u32],
        marks: &[&str],
    ) -> mail::Result<()> {
        for (done, mark) in marks.iter().enumerate() {
            if let Err(err) = self.mail.add_keyword(folder, uids, mark).await {
                self.clear_marks(folder, uids, &marks[..done]).await;
                return Err(err);
            }
        }
        Ok(())
    }

    /// Clears `marks` as far as it can. If the connection is gone and this
    /// fails too, the journal has no move for the message, so the next pass
    /// handles it again.
    pub(super) async fn clear_marks(&mut self, folder: &str, uids: &[u32], marks: &[&str]) {
        for mark in marks {
            if let Err(err) = self.mail.remove_keyword(folder, uids, mark).await {
                tracing::debug!(%err, mark, "could not clear a mark");
            }
        }
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
            for keyword in &pending.keywords {
                self.mail
                    .remove_keyword(&pending.source, &left, keyword)
                    .await
                    .context(&context)?;
            }
        }
        self.store.drop_move(pending.decision_id).context(&context)
    }
}
