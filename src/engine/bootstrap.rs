use std::time::Duration;

use jiff::Timestamp;

use crate::auth::Tally;
use crate::classify::Label;
use crate::mail::{Mailbox, State};

use super::{Context, Engine, FETCH_BATCH, Result, training_key};

/// A folder to learn from and how many of its most recent messages to take.
#[derive(Clone, Debug)]
pub struct Source {
    pub folder: String,
    pub limit: usize,
}

/// Says what to learn from.
#[derive(Clone, Debug, Default)]
pub struct BootstrapOptions {
    /// How many recent sent messages to whitelist recipients from; zero skips
    /// the whitelist.
    pub sent_limit: usize,
    /// Trained as spam.
    pub spam: Vec<Source>,
    /// Trained as good, but only messages that are read and at least
    /// `good_min_age` old: unread or recent mail may be spam nobody has looked
    /// at yet.
    pub good: Vec<Source>,
    pub good_min_age: Duration,
    /// Leaves the watched folders' cursors alone, so a bootstrap of some
    /// folders doesn't skip mail that arrived unjudged in others.
    pub keep_cursors: bool,
    /// Re-learns messages that were already learnt: each is untrained and
    /// trained again with today's tokens, so a change to the tokenizer reaches
    /// what was learnt before. Their label is the folder's.
    pub refresh: bool,
}

/// Counts what bootstrap learnt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BootstrapSummary {
    pub addresses: usize,
    pub spam: usize,
    pub good: usize,
    /// Messages that had been learnt and were learnt again.
    pub refreshed: usize,
    /// The provider's domain, when the sample named one.
    pub auth_server_id: Option<String>,
}

/// What bootstrap gathers while it trains.
#[derive(Default)]
struct Run {
    tally: Tally,
    refreshed: usize,
}

impl<M: Mailbox> Engine<M> {
    /// Seeds the whitelist from Sent and the model from the given folders,
    /// then starts watching from current mail.
    ///
    /// # Errors
    /// Fails when the mailbox or the store does; what was learnt before the
    /// failure stays learnt.
    pub async fn bootstrap(&mut self, opts: &BootstrapOptions) -> Result<BootstrapSummary> {
        let mut summary = BootstrapSummary::default();
        let mut run = Run::default();

        summary.addresses = self.whitelist_sent(opts.sent_limit).await?;

        // Good mail first: the recipient status of each message depends on
        // which addresses good mail has gone to, and spam should be seen as
        // live mail would be.
        for source in &opts.good {
            let before = Timestamp::now()
                .checked_sub(opts.good_min_age)
                .unwrap_or(Timestamp::MIN);
            let uids = self.mail.seen_before(&source.folder, before).await;
            summary.good += self
                .train_source(source, Label::Good, opts.refresh, &mut run, uids)
                .await?;
        }
        for source in &opts.spam {
            let uids = self.mail.uids_after(&source.folder, 0).await;
            summary.spam += self
                .train_source(source, Label::Spam, opts.refresh, &mut run, uids)
                .await?;
        }

        summary.refreshed = run.refreshed;
        if opts.refresh {
            self.store
                .set_tokenizer_version(crate::classify::TOKENIZER_VERSION)
                .context("bootstrap")?;
        }
        summary.auth_server_id = self.learn_auth_server_id(&run.tally)?;

        if !opts.keep_cursors {
            self.start_watching().await?;
        }
        Ok(summary)
    }

    /// Starts every watched folder from its current mail.
    async fn start_watching(&mut self) -> Result<()> {
        for folder in self.cfg.folders.watch.clone() {
            let state: State = self
                .mail
                .state(&folder)
                .await
                .context(format!("bootstrap {folder}"))?;
            self.start_at_current(&folder, state)?;
        }
        Ok(())
    }

    /// Trains the latest of the messages `candidates` found.
    async fn train_source(
        &mut self,
        source: &Source,
        label: Label,
        refresh: bool,
        run: &mut Run,
        candidates: crate::mail::Result<Vec<u32>>,
    ) -> Result<usize> {
        let context = format!("bootstrap from {}", source.folder);
        let uids = candidates.context(&context)?;
        let picked = latest(&uids, source.limit);
        tracing::info!(folder = %source.folder, label = label.as_str(), messages = picked.len(), "training");

        self.train_corpus(&source.folder, picked, label, refresh, run)
            .await
    }

    async fn whitelist_sent(&mut self, limit: usize) -> Result<usize> {
        if limit == 0 {
            return Ok(0);
        }

        let sent = self.cfg.folders.sent.clone();
        let context = format!("bootstrap from {sent}");
        let uids = self.mail.uids_after(&sent, 0).await.context(&context)?;
        let uids = latest(&uids, limit);
        tracing::info!(folder = %sent, messages = uids.len(), "whitelisting recipients");

        let recipients = self.mail.recipients(&sent, uids).await.context(&context)?;
        Ok(self.whitelist_recipients(&recipients)?.len())
    }

    async fn train_corpus(
        &mut self,
        folder: &str,
        uids: &[u32],
        label: Label,
        refresh: bool,
        run: &mut Run,
    ) -> Result<usize> {
        let context = format!("bootstrap from {folder}");
        let mut trained = 0;

        for batch in uids.chunks(FETCH_BATCH) {
            for message in self.mail.fetch(folder, batch).await.context(&context)? {
                run.tally.add(&message.raw);

                if refresh {
                    // Before the tokens are worked out, so the recipient
                    // status doesn't count the message's own earlier training.
                    let untrained = self
                        .store
                        .untrain(&training_key(&message))
                        .context(&context)?;
                    run.refreshed += usize::from(untrained);
                }

                let tokens = self.tokens(&message.raw).context(&context)?;
                if self
                    .store
                    .train(&training_key(&message), &tokens, label)
                    .context(&context)?
                {
                    trained += 1;
                }
            }
            tracing::info!(
                folder,
                done = trained,
                total = uids.len(),
                "training progress"
            );
        }
        Ok(trained)
    }

    /// Stores the provider's domain when the sample names one, and returns it.
    /// A sample that can't tell leaves what was learnt before.
    fn learn_auth_server_id(&mut self, tally: &Tally) -> Result<Option<String>> {
        let Some(domain) = tally.infer() else {
            return Ok(None);
        };

        // The domain first: if the second write fails, the value is stale,
        // which is safe, and not wrongly trusted for a new host.
        self.store
            .set_setting(crate::auth_pin::AUTH_SETTING, &domain)
            .context("learn authserv-id")?;
        self.store
            .set_setting(crate::auth_pin::AUTH_HOST_SETTING, &self.cfg.account.host)
            .context("learn authserv-id")?;
        self.auth_id = None;
        tracing::info!(%domain, sample = tally.total(), "learnt the provider's authserv-id");
        Ok(Some(domain))
    }
}

/// The `n` highest (most recent) of ascending UIDs.
fn latest(uids: &[u32], n: usize) -> &[u32] {
    &uids[uids.len() - n.min(uids.len())..]
}
