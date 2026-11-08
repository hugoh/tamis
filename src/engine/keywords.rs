use std::time::Duration;

use jiff::Timestamp;

use crate::classify::Label;
use crate::mail::{Mailbox, Message, ModState};
use crate::store::ModSeq;

use super::domain::{Reason, Summary};
use super::{Context, Engine, Result};

/// Bounds how far back keyword marks are looked for, so each pass's searches
/// stay small.
const KEYWORD_WINDOW: Duration = Duration::from_hours(7 * 24);

/// The keywords mail clients set for Junk and Not Junk: the IANA-registered
/// ones and Apple Mail's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mark {
    Spam,
    Good,
}

impl Mark {
    pub(super) fn keywords(self) -> &'static [&'static str] {
        match self {
            Self::Spam => &["$Junk", "Junk"],
            Self::Good => &["$NotJunk", "NotJunk"],
        }
    }

    fn label(self) -> Label {
        match self {
            Self::Spam => Label::Spam,
            Self::Good => Label::Good,
        }
    }
}

/// Whether `flags` (lower-cased, as a message has them) include any of
/// `wanted`, compared case-insensitively.
pub(super) fn has_any_flag(flags: &[String], wanted: &[&str]) -> bool {
    wanted
        .iter()
        .any(|flag| flags.contains(&flag.to_lowercase()))
}

impl<M: Mailbox> Engine<M> {
    /// Learns from Junk / Not Junk marks that contradict tamis's own verdict
    /// on recent mail in the watched folders and Junk. Marks that agree, or on
    /// mail tamis never judged, may come from a client's own filter and are
    /// ignored.
    pub(super) async fn learn_from_keywords(&mut self, summary: &mut Summary) -> Result<()> {
        let since = Timestamp::now()
            .checked_sub(KEYWORD_WINDOW)
            .unwrap_or(Timestamp::MIN);
        let mut folders = self.cfg.folders.watch.clone();
        folders.push(self.cfg.folders.junk.clone());

        for folder in folders {
            self.check_stop()?;
            self.learn_folder_keywords(&folder, since, summary).await?;
        }
        Ok(())
    }

    /// Searches one folder for marks. With CONDSTORE it only looks at flags
    /// changed since its last search, then saves how far it got.
    async fn learn_folder_keywords(
        &mut self,
        folder: &str,
        since: Timestamp,
        summary: &mut Summary,
    ) -> Result<()> {
        let context = format!("search {folder} for marks");
        let current = self.mail.mod_state(folder).await.context(&context)?;
        let changed_since = self.changed_since(folder, current)?;

        if matches!(current, Some(now) if changed_since > 0 && changed_since == now.highest_mod_seq)
        {
            return Ok(());
        }

        for mark in [Mark::Spam, Mark::Good] {
            let keywords: Vec<String> = mark.keywords().iter().map(|k| (*k).to_owned()).collect();
            let uids = self
                .mail
                .search_keywords(folder, &keywords, since, changed_since)
                .await
                .context(format!("search {folder} for {keywords:?}"))?;
            self.learn_marked(folder, &uids, mark, summary).await?;
        }

        if let Some(current) = current {
            let saved = ModSeq {
                uid_validity: current.uid_validity,
                highest: current.highest_mod_seq,
            };
            self.store.set_mod_seq(folder, saved).context(&context)?;
        }
        Ok(())
    }

    /// The mod-sequence the folder's last search reached, or 0 to search every
    /// flag: without CONDSTORE, on the first search, or after `UIDVALIDITY`
    /// changed.
    fn changed_since(&self, folder: &str, current: Option<ModState>) -> Result<u64> {
        let Some(current) = current else {
            return Ok(0);
        };
        let saved = self
            .store
            .mod_seq(folder)
            .context(format!("search {folder} for marks"))?;
        Ok(saved
            .filter(|s| s.uid_validity == current.uid_validity)
            .map_or(0, |s| s.highest))
    }

    /// Decides from headers alone which marked messages to learn, and
    /// downloads only those.
    async fn learn_marked(
        &mut self,
        folder: &str,
        uids: &[u32],
        mark: Mark,
        summary: &mut Summary,
    ) -> Result<()> {
        let context = format!("fetch marked mail in {folder}");
        let envelopes = self
            .mail
            .fetch_envelopes(folder, uids)
            .await
            .context(&context)?;

        let mut learn = Vec::new();
        for envelope in &envelopes {
            if self.mark_contradicts(envelope, mark)? {
                learn.push(envelope.uid);
            }
        }
        if learn.is_empty() {
            return Ok(());
        }

        for message in self.mail.fetch(folder, &learn).await.context(&context)? {
            self.learn_mark(folder, &message, mark)?;
            summary.corrected += 1;
        }
        Ok(())
    }

    /// Whether a message marked `mark` was last judged otherwise by tamis in a
    /// watched folder. Messages marked both ways are ambiguous and skipped.
    fn mark_contradicts(&self, message: &Message, mark: Mark) -> Result<bool> {
        if has_any_flag(&message.flags, Mark::Spam.keywords())
            && has_any_flag(&message.flags, Mark::Good.keywords())
        {
            return Ok(false);
        }
        let decision = self
            .store
            .latest_decision(&message.message_id)
            .context("look up decision")?;
        let Some(decision) = decision.filter(|d| self.cfg.folders.watch.contains(&d.mailbox))
        else {
            return Ok(false);
        };

        let verdict = decision.verdict.as_str();
        Ok(match mark {
            Mark::Spam => matches!(verdict, "good" | "uncertain"),
            Mark::Good => matches!(verdict, "spam" | "uncertain"),
        })
    }

    fn learn_mark(&self, folder: &str, message: &Message, mark: Mark) -> Result<()> {
        let tokens = self.tokens(&message.raw)?;
        match mark.label() {
            Label::Good => self.learn_good(folder, message, &tokens, Reason::Correction),
            Label::Spam => self.learn_spam(folder, message, &tokens, Reason::Correction),
        }
    }
}
