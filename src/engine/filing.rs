use crate::config::Mode;
use crate::mail::{Mailbox, Message};
use crate::rules;

use super::domain::{Action, Judgement, Reason, Verdict, keyword};
use super::{Context, Engine, Result};

/// What the rules decided for a message.
pub(super) struct RulePlan {
    target: Option<String>,
    flags: Vec<String>,
}

impl<M: Mailbox> Engine<M> {
    /// What the rules do to a good message: the folder to file it into and
    /// the flags to set, or `None` to leave it alone. A script that fails on
    /// one message must not stall the pass, so that message is left where it
    /// is.
    pub(super) async fn rule_plan(
        &mut self,
        folder: &str,
        message: &Message,
        verdict: &Judgement,
    ) -> Option<RulePlan> {
        if verdict.verdict != Verdict::Good {
            return None;
        }
        let rules = self.rules.as_ref()?;

        let decision = match rules.decide(&message.raw, &message.from) {
            Ok(decision) => decision,
            Err(err) => {
                tracing::warn!(folder, uid = message.uid, %err, "rules failed");
                return None;
            }
        };

        let target = match decision.folder {
            Some(target) if target != folder => {
                if !self.target_usable(folder, message, &target).await {
                    return None;
                }
                Some(target)
            }
            _ => None,
        };
        let flags: Vec<String> = decision
            .flags
            .into_iter()
            .filter(|flag| !message.has_flag(flag))
            .collect();
        (target.is_some() || !flags.is_empty()).then_some(RulePlan { target, flags })
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

    /// Carries out what the rules decided for a good message; in shadow mode
    /// it only records what it would have done. The flags are set before the
    /// move, so they travel with the message, and are cleared again if the
    /// move fails.
    pub(super) async fn file_by_rule(
        &mut self,
        folder: &str,
        message: &Message,
        mut verdict: Judgement,
        plan: &RulePlan,
    ) -> Result<()> {
        verdict.reason = Reason::Rule;
        let live = self.cfg.policy.mode == Mode::Live;
        let flags: Vec<&str> = plan.flags.iter().map(String::as_str).collect();

        let Some(target) = &plan.target else {
            let action = Action::Keyword(plan.flags.join(" "));
            if live {
                self.add_marks(folder, &[message.uid], &flags)
                    .await
                    .context("flag by rule")?;
            }
            return self.record(folder, message, &verdict, &action, live);
        };

        let action = Action::Move(target.clone());
        if !live {
            return self.record(folder, message, &verdict, &action, false);
        }

        self.mail.ensure(target).await.context("file by rule")?;
        let marks: Vec<&str> = std::iter::once(keyword::SORTED).chain(flags).collect();
        self.move_guarded(folder, message, &verdict, target, &marks, "file by rule")
            .await
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
