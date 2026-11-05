use crate::config::Mode;
use crate::mail::{Mailbox, Message};
use crate::rules;

use super::domain::{Action, Judgement, Reason, Verdict, keyword};
use super::{Context, Engine, Result};

impl<M: Mailbox> Engine<M> {
    /// The folder the rules file a good message into, or `None` to leave it.
    /// A script that fails on one message must not stall the pass, so that
    /// message is left where it is.
    pub(super) async fn rule_target(
        &mut self,
        folder: &str,
        message: &Message,
        verdict: &Judgement,
    ) -> Option<String> {
        if verdict.verdict != Verdict::Good {
            return None;
        }
        let rules = self.rules.as_ref()?;

        let target = match rules.folder(&message.raw, &message.from) {
            Ok(target) => target?,
            Err(err) => {
                tracing::warn!(folder, uid = message.uid, %err, "rules failed");
                return None;
            }
        };

        (target != folder && self.target_usable(folder, message, &target).await).then_some(target)
    }

    /// False for a folder that is missing and may not be created. A failed
    /// lookup counts as unusable: it leaves the message where it is.
    async fn target_usable(&mut self, folder: &str, message: &Message, target: &str) -> bool {
        if self.cfg.rules.create_folders {
            return true;
        }
        match self.mail.exists(target).await {
            Ok(true) => true,
            Ok(false) => {
                tracing::warn!(
                    folder,
                    uid = message.uid,
                    target,
                    "rules: no such folder; leaving the message"
                );
                false
            }
            Err(err) => {
                tracing::warn!(folder, uid = message.uid, target, %err, "rules: look up folder");
                false
            }
        }
    }

    /// Moves a good message to `target`; in shadow mode it only records what
    /// it would have done.
    pub(super) async fn file_by_rule(
        &mut self,
        folder: &str,
        message: &Message,
        mut verdict: Judgement,
        target: &str,
    ) -> Result<()> {
        verdict.reason = Reason::Rule;
        let action = Action::Move(target.to_owned());
        if self.cfg.policy.mode != Mode::Live {
            return self.record(folder, message, &verdict, &action, false);
        }

        let uids = [message.uid];
        self.mail.ensure(target).await.context("file by rule")?;
        self.mail
            .add_keyword(folder, &uids, keyword::SORTED)
            .await
            .context("file by rule")?;
        let intent = self.intend_move(folder, message, &verdict, target, keyword::SORTED)?;
        if let Err(err) = self.mail.move_messages(folder, &uids, target).await {
            // Take the keyword back off. If the connection is gone and this
            // fails too, the journal has no move for the message, so the next
            // pass files it anyway.
            if let Err(undo) = self
                .mail
                .remove_keyword(folder, &uids, keyword::SORTED)
                .await
            {
                tracing::debug!(%undo, "could not clear the sorted keyword");
            }
            self.withdraw_move(intent, &err);
            return Err(err).context("file by rule");
        }
        self.settle_move(intent, folder, message, &verdict, target)
    }

    /// Logs each folder the rules file into that the mailbox lacks, so a typo
    /// shows when tamis starts and not only when a message first matches. It
    /// says nothing when missing folders are created.
    ///
    /// # Errors
    /// Fails when the folders can't be listed.
    pub async fn warn_missing_rule_folders(&mut self) -> Result<()> {
        let Some(rules) = &self.rules else {
            return Ok(());
        };
        if self.cfg.rules.create_folders {
            return Ok(());
        }

        let existing = self.mail.folders().await.context("check rule folders")?;
        for absent in rules::missing(rules.targets(), &existing) {
            tracing::warn!(
                target = %absent.folder,
                nearest = absent.nearest.as_deref().unwrap_or(""),
                "rules file into a folder that doesn't exist; matching mail stays put"
            );
        }
        Ok(())
    }
}
