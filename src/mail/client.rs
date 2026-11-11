//! The real IMAP connection: `imap-client` over TLS (or plaintext, for local
//! test servers), implementing [`Mailbox`].

use std::collections::HashMap;
use std::num::{NonZeroU32, NonZeroU64};
use std::time::Duration;

use imap_client::client::tokio::{Client as Imap, ClientError};
use imap_client::imap_types::IntoStatic;
use imap_client::imap_types::core::{AString, Atom, IString, Quoted, Vec1};
use imap_client::imap_types::fetch::{
    MacroOrMessageDataItemNames, MessageDataItem, MessageDataItemName,
};
use imap_client::imap_types::flag::{Flag, FlagPerm, StoreType};
use imap_client::imap_types::mailbox::Mailbox as ImapMailbox;
use imap_client::imap_types::search::SearchKey;
use imap_client::imap_types::secret::Secret;
use imap_client::imap_types::sequence::SequenceSet;
use imap_client::imap_types::status::StatusDataItemName;
use imap_client::tasks::tasks::TaskError;
use imap_client::tasks::tasks::login::LoginTask;
use jiff::Timestamp;
use tokio::time::timeout;

use super::convert::{
    address_of, bracket, date, invalid, keyword_flag, mailbox_name, text, to_message, uid_set,
};
use super::tasks::{StatusTask, StatusValues, UidExpungeTask};
use super::utf7;
use super::{Error, MAX_MESSAGE_BYTES, Mailbox, Message, ModState, Result, State};

/// How long one command may take before the connection is given up on.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
/// An IDLE that ends sooner than this was refused, not interrupted.
const IDLE_MIN: Duration = Duration::from_millis(50);
/// An IDLE that ended this much before its renewal time was ended by news.
const IDLE_SLACK: Duration = Duration::from_millis(500);
/// How long connecting and logging in may take.
const DIAL_TIMEOUT: Duration = Duration::from_secs(30);

/// How to connect.
#[derive(Clone)]
pub struct DialConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// Dials without TLS. Only for local test servers.
    pub plaintext: bool,
}

impl std::fmt::Debug for DialConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DialConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("plaintext", &self.plaintext)
            .finish_non_exhaustive()
    }
}

/// A logged-in IMAP connection. Not safe for concurrent use.
pub struct Client {
    imap: Imap,
    selected: Option<String>,
    avoid_move: bool,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

fn map_error(err: ClientError) -> Error {
    match err {
        ClientError::ResolveTask(TaskError::UnexpectedNoResponse(status)) => {
            Error::Server(status.text.to_string())
        }
        ClientError::ResolveTask(other) => Error::Server(other.to_string()),
        ClientError::Validation(err) => Error::Server(format!("invalid argument: {err}")),
        other => Error::Connection(format!("{other}: {}", source_chain(&other))),
    }
}

/// What a refused `LOGIN` means. A server at its connection limit refuses the
/// same way as one that dislikes the password, but waiting and retrying
/// normally can fix it, so it is not a [`Error::Login`].
fn login_error(err: ClientError) -> Error {
    let mapped = map_error(err);
    let text = mapped.to_string().to_lowercase();
    let at_limit = ["too many", "simultaneous", "number of connections"]
        .iter()
        .any(|phrase| text.contains(phrase));
    if at_limit {
        mapped
    } else {
        Error::Login(mapped.to_string())
    }
}

fn source_chain(err: &dyn std::error::Error) -> String {
    let mut parts = Vec::new();
    let mut source = err.source();
    while let Some(next) = source {
        parts.push(next.to_string());
        source = next.source();
    }
    parts.join(": ")
}

/// Runs a command with a deadline; a command that doesn't finish leaves the
/// connection unusable.
async fn within<T>(
    operation: impl Future<Output = std::result::Result<T, ClientError>>,
) -> Result<T> {
    match timeout(COMMAND_TIMEOUT, operation).await {
        Ok(result) => result.map_err(map_error),
        Err(_) => Err(Error::Connection("the server stopped answering".to_owned())),
    }
}

/// What a read-only `SELECT` reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Examined {
    pub messages: u32,
    pub uid_validity: u32,
    pub uid_next: u32,
    /// `PERMANENTFLAGS` includes `\*`: the server keeps keywords it was never told about.
    pub keywords_allowed: bool,
}

/// A string as credentials are sent: quoted, which every server
/// reads, rather than as a bare atom, which some read differently. Strings a
/// quoted string can't hold (line breaks) go as literals.
fn quoted(value: &str) -> Result<AString<'static>> {
    match Quoted::try_from(value.to_owned()) {
        Ok(quoted) => Ok(AString::String(IString::Quoted(quoted))),
        Err(_) => AString::try_from(value.to_owned())
            .map_err(|_| Error::Server("invalid credentials".to_owned())),
    }
}

impl Client {
    /// Connects and logs in.
    ///
    /// # Errors
    /// [`Error::Login`] when the credentials are rejected, [`Error::Connection`]
    /// when the server can't be reached.
    pub async fn dial(config: &DialConfig) -> Result<Self> {
        crate::tls::init();
        let connect = async {
            let mut imap = if config.plaintext {
                Imap::insecure(&config.host, config.port).await
            } else {
                Imap::rustls(&config.host, config.port, false, None).await
            }?;

            let (Ok(user), Ok(password)) = (quoted(&config.username), quoted(&config.password))
            else {
                return Ok(Err(ClientError::ResolveTask(TaskError::MissingData(
                    "credentials".to_owned(),
                ))));
            };
            tracing::debug!(user = %config.username, "logging in");
            // The task rather than `login()`, which would send bare atoms.
            let login = imap
                .resolve(LoginTask::new(user, Secret::new(password)))
                .await;
            match login {
                Ok(Ok(_)) => imap.refresh_capabilities().await?,
                Ok(Err(err)) => return Ok(Err(ClientError::ResolveTask(err))),
                Err(err) => return Ok(Err(err)),
            }
            if imap.state.ext_condstore_supported() {
                imap.enable_condstore_if_supported().await?;
            }
            Ok(Ok(imap))
        };

        let imap = match timeout(DIAL_TIMEOUT, connect).await {
            Err(_) => return Err(Error::Connection("timed out connecting".to_owned())),
            Ok(Err(err)) => return Err(map_error(err)),
            Ok(Ok(Err(err))) => return Err(login_error(err)),
            Ok(Ok(Ok(imap))) => imap,
        };
        Ok(Self {
            imap,
            selected: None,
            avoid_move: false,
        })
    }

    /// Logs out and closes the connection. Errors are ignored: the connection
    /// is going away either way.
    pub async fn close(mut self) {
        let _ = timeout(
            Duration::from_secs(5),
            self.imap
                .resolve(imap_client::tasks::tasks::logout::LogoutTask::new()),
        )
        .await;
    }

    /// Adds a message to `mailbox` and returns its UID (`None` if the server
    /// doesn't report one).
    ///
    /// # Errors
    /// Fails when the server refuses the message.
    pub async fn append(
        &mut self,
        mailbox: &str,
        raw: &[u8],
        flags: &[&str],
    ) -> Result<Option<u32>> {
        let flags = flags
            .iter()
            .map(|f| keyword_flag(f))
            .collect::<Result<Vec<_>>>()?;
        let appended = within(self.imap.appenduid(utf7::encode(mailbox), flags, raw)).await?;
        Ok(appended.map(|(uid, _validity)| uid.get()))
    }

    /// The capabilities the server advertises, as it spelled them.
    #[must_use]
    pub fn capabilities(&self) -> Vec<String> {
        self.imap
            .state
            .capabilities_iter()
            .map(ToString::to_string)
            .collect()
    }

    /// Every folder with its attributes, such as `\\Junk`.
    ///
    /// # Errors
    /// Fails when the server refuses the listing.
    pub async fn list_with_attributes(&mut self) -> Result<Vec<(String, Vec<String>)>> {
        let listed = within(self.imap.list("", "*")).await?;
        Ok(listed
            .into_iter()
            .map(|(mailbox, _, attributes)| {
                (
                    mailbox_name(&mailbox),
                    attributes.iter().map(ToString::to_string).collect(),
                )
            })
            .collect())
    }

    /// Opens `mailbox` read-only and says what the server reported.
    ///
    /// # Errors
    /// Fails when the folder can't be opened.
    pub async fn examine(&mut self, mailbox: &str) -> Result<Examined> {
        self.selected = None;
        let data = within(self.imap.examine(utf7::encode(mailbox))).await?;
        Ok(Examined {
            messages: data.exists.unwrap_or(0),
            uid_validity: data.uid_validity.map_or(0, NonZeroU32::get),
            uid_next: data.uid_next.map_or(0, NonZeroU32::get),
            keywords_allowed: data
                .permanent_flags
                .as_ref()
                .is_some_and(|flags| flags.iter().any(|flag| matches!(flag, FlagPerm::Asterisk))),
        })
    }

    /// Creates `mailbox`, failing if it exists.
    ///
    /// # Errors
    /// Fails when the server refuses.
    pub async fn create_folder(&mut self, mailbox: &str) -> Result<()> {
        within(self.imap.create(utf7::encode(mailbox))).await
    }

    /// Deletes `mailbox`.
    ///
    /// # Errors
    /// Fails when the server refuses.
    pub async fn delete_folder(&mut self, mailbox: &str) -> Result<()> {
        if self.selected.as_deref() == Some(mailbox) {
            self.selected = None;
        }
        within(self.imap.delete(utf7::encode(mailbox))).await
    }

    /// How a move would be done on this server.
    #[must_use]
    pub fn move_method(&self) -> &'static str {
        if self.supports_move() {
            "native MOVE"
        } else if self.supports_uidplus() {
            "fallback COPY + STORE + UID EXPUNGE"
        } else {
            "unsafe: neither MOVE nor UIDPLUS"
        }
    }

    /// Whether the server offers IDLE.
    #[must_use]
    pub fn supports_idle(&self) -> bool {
        self.imap.state.ext_idle_supported()
    }

    /// Selects `mailbox` and waits in IDLE until the server reports a change,
    /// or until `renew` has passed, when IDLE is ended and restarted so it
    /// stays inside the limit of RFC 2177. Returns whether something changed.
    ///
    /// # Errors
    /// Fails when the connection does, or when IDLE ends at once, which a
    /// server that refuses it does and which would otherwise spin.
    pub async fn idle(&mut self, mailbox: &str, renew: Duration) -> Result<bool> {
        self.select(mailbox).await?;
        self.imap.state.set_idle_timeout(renew);

        let tag = self.imap.enqueue_idle();
        let started = std::time::Instant::now();
        let outcome = self.imap.idle(tag).await;
        let waited = started.elapsed();

        outcome.map_err(|err| Error::Connection(format!("idle: {err}")))?;
        if waited < IDLE_MIN {
            return Err(Error::Server("the server ended IDLE at once".to_owned()));
        }
        Ok(waited + IDLE_SLACK < renew)
    }

    /// Moves with COPY, `\Deleted` and `UID EXPUNGE` even where the server
    /// has MOVE, for servers where MOVE misbehaves and for tests.
    pub fn avoid_move(&mut self) {
        self.avoid_move = true;
    }

    fn supports_move(&self) -> bool {
        !self.avoid_move && self.imap.state.ext_move_supported()
    }

    fn supports_uidplus(&self) -> bool {
        self.imap.state.ext_uidplus_supported()
    }

    /// Selects `mailbox` unless it already is.
    async fn select(&mut self, mailbox: &str) -> Result<()> {
        if self.selected.as_deref() == Some(mailbox) {
            return Ok(());
        }
        self.selected = None;
        within(self.imap.select(utf7::encode(mailbox))).await?;
        self.selected = Some(mailbox.to_owned());
        Ok(())
    }

    async fn status(
        &mut self,
        mailbox: &str,
        items: Vec<StatusDataItemName>,
    ) -> Result<StatusValues> {
        let mailbox = ImapMailbox::try_from(utf7::encode(mailbox))
            .map_err(invalid)?
            .into_static();
        let task = StatusTask::new(mailbox, items);
        let outcome = within(async { self.imap.resolve(task).await }).await?;
        outcome.map_err(|err| Error::Server(err.to_string()))
    }

    async fn uid_search(
        &mut self,
        mailbox: &str,
        criteria: Vec1<SearchKey<'static>>,
    ) -> Result<Vec<u32>> {
        self.select(mailbox).await?;
        let mut uids: Vec<u32> = within(self.imap.uid_search(criteria))
            .await?
            .into_iter()
            .map(NonZeroU32::get)
            .collect();
        uids.sort_unstable();
        Ok(uids)
    }

    async fn fetch_items(
        &mut self,
        mailbox: &str,
        uids: &[u32],
        items: Vec<MessageDataItemName<'static>>,
    ) -> Result<HashMap<NonZeroU32, Vec1<MessageDataItem<'static>>>> {
        // An empty UID set can't be encoded.
        if uids.is_empty() {
            return Ok(HashMap::new());
        }
        self.select(mailbox).await?;
        let set = uid_set(uids)?;
        within(self.imap.uid_fetch(
            set,
            MacroOrMessageDataItemNames::MessageDataItemNames(items),
        ))
        .await
    }

    async fn fetch_messages(
        &mut self,
        mailbox: &str,
        uids: &[u32],
        with_body: bool,
    ) -> Result<Vec<Message>> {
        let mut items = vec![
            MessageDataItemName::Uid,
            MessageDataItemName::Flags,
            MessageDataItemName::Envelope,
        ];
        if with_body {
            items.push(MessageDataItemName::BodyExt {
                section: None,
                partial: Some((
                    0,
                    NonZeroU32::new(u32::try_from(MAX_MESSAGE_BYTES).unwrap_or(u32::MAX))
                        .unwrap_or(NonZeroU32::MIN),
                )),
                peek: true,
            });
        }

        let fetched = self.fetch_items(mailbox, uids, items).await?;
        let mut messages: Vec<Message> = fetched
            .into_iter()
            .map(|(uid, items)| to_message(uid.get(), items.as_ref()))
            .collect();
        messages.sort_by_key(|m| m.uid);
        Ok(messages)
    }
}

impl Mailbox for Client {
    async fn state(&mut self, mailbox: &str) -> Result<State> {
        let values = self
            .status(
                mailbox,
                vec![StatusDataItemName::UidNext, StatusDataItemName::UidValidity],
            )
            .await?;
        match (values.uid_validity, values.uid_next) {
            (Some(uid_validity), Some(uid_next)) => Ok(State {
                uid_validity: uid_validity.get(),
                uid_next: uid_next.get(),
            }),
            _ => Err(Error::Server(format!(
                "status {mailbox}: the server sent no UIDNEXT or UIDVALIDITY"
            ))),
        }
    }

    async fn messages(&mut self, mailbox: &str) -> Result<u32> {
        let values = self
            .status(mailbox, vec![StatusDataItemName::Messages])
            .await?;
        values.messages.ok_or_else(|| {
            Error::Server(format!(
                "status {mailbox}: the server sent no message count"
            ))
        })
    }

    async fn uids_after(&mut self, mailbox: &str, after: u32) -> Result<Vec<u32>> {
        let from = NonZeroU32::new(after.saturating_add(1)).unwrap_or(NonZeroU32::MIN);
        let criteria = Vec1::from(SearchKey::Uid(
            SequenceSet::try_from(format!("{from}:*").as_str()).map_err(invalid)?,
        ));
        let mut uids = self.uid_search(mailbox, criteria).await?;
        // "n:*" matches the highest UID even when it is below n.
        uids.retain(|uid| *uid > after);
        Ok(uids)
    }

    async fn seen_before(&mut self, mailbox: &str, before: Timestamp) -> Result<Vec<u32>> {
        let criteria = Vec1::try_from(vec![SearchKey::Seen, SearchKey::Before(date(before))])
            .map_err(invalid)?;
        self.uid_search(mailbox, criteria).await
    }

    async fn find_message_id(&mut self, mailbox: &str, message_id: &str) -> Result<Vec<u32>> {
        if message_id.is_empty() {
            return Ok(Vec::new());
        }
        let key = SearchKey::Header(
            AString::try_from("Message-ID".to_owned()).map_err(invalid)?,
            AString::try_from(message_id.to_owned()).map_err(invalid)?,
        );
        self.uid_search(mailbox, Vec1::from(key)).await
    }

    async fn mod_state(&mut self, mailbox: &str) -> Result<Option<ModState>> {
        if !self.imap.state.ext_condstore_supported() {
            return Ok(None);
        }
        let values = self
            .status(
                mailbox,
                vec![
                    StatusDataItemName::UidValidity,
                    StatusDataItemName::HighestModSeq,
                ],
            )
            .await?;
        Ok(match (values.uid_validity, values.highest_mod_seq) {
            (Some(uid_validity), Some(highest)) => Some(ModState {
                uid_validity: uid_validity.get(),
                highest_mod_seq: highest,
            }),
            _ => None,
        })
    }

    async fn search_keywords(
        &mut self,
        mailbox: &str,
        keywords: &[String],
        since: Timestamp,
        changed_since: u64,
    ) -> Result<Vec<u32>> {
        let mut any: Option<SearchKey<'static>> = None;
        for keyword in keywords {
            let atom = Atom::try_from(keyword.clone()).map_err(invalid)?;
            let key = SearchKey::Keyword(atom);
            any = Some(match any {
                None => key,
                Some(previous) => SearchKey::Or(Box::new(previous), Box::new(key)),
            });
        }
        let Some(any) = any else {
            return Ok(Vec::new());
        };

        let mut criteria = vec![any, SearchKey::Since(date(since))];
        if let Some(modseq) =
            NonZeroU64::new(changed_since.saturating_add(1)).filter(|_| changed_since > 0)
        {
            // MODSEQ n matches mod-sequences of n or more.
            criteria.push(SearchKey::ModSequence {
                entry: None,
                modseq: modseq.get(),
            });
        }
        self.uid_search(mailbox, Vec1::try_from(criteria).map_err(invalid)?)
            .await
    }

    async fn fetch(&mut self, mailbox: &str, uids: &[u32]) -> Result<Vec<Message>> {
        self.fetch_messages(mailbox, uids, true).await
    }

    async fn fetch_envelopes(&mut self, mailbox: &str, uids: &[u32]) -> Result<Vec<Message>> {
        self.fetch_messages(mailbox, uids, false).await
    }

    async fn recipients(&mut self, mailbox: &str, uids: &[u32]) -> Result<Vec<String>> {
        let fetched = self
            .fetch_items(
                mailbox,
                uids,
                vec![MessageDataItemName::Uid, MessageDataItemName::Envelope],
            )
            .await?;

        let mut ordered: Vec<_> = fetched.into_iter().collect();
        ordered.sort_by_key(|(uid, _)| *uid);

        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for (_, items) in ordered {
            for item in items.as_ref() {
                let MessageDataItem::Envelope(envelope) = item else {
                    continue;
                };
                for address in envelope.to.iter().chain(&envelope.cc).map(address_of) {
                    if !address.is_empty() && seen.insert(address.clone()) {
                        out.push(address);
                    }
                }
            }
        }
        Ok(out)
    }

    async fn move_messages(&mut self, mailbox: &str, uids: &[u32], dest: &str) -> Result<()> {
        if !self.supports_move() && !self.supports_uidplus() {
            return Err(Error::UnsafeMove);
        }
        if uids.is_empty() {
            return Ok(());
        }
        self.select(mailbox).await?;

        if self.supports_move() {
            let set = uid_set(uids)?;
            return within(self.imap.uid_move(set, utf7::encode(dest))).await;
        }
        self.copy_delete(mailbox, uids, dest).await
    }

    async fn add_keyword(&mut self, mailbox: &str, uids: &[u32], keyword: &str) -> Result<()> {
        self.store_keyword(mailbox, uids, keyword, StoreType::Add)
            .await
    }

    async fn remove_keyword(&mut self, mailbox: &str, uids: &[u32], keyword: &str) -> Result<()> {
        self.store_keyword(mailbox, uids, keyword, StoreType::Remove)
            .await
    }

    async fn ensure(&mut self, mailbox: &str) -> Result<()> {
        if self.exists(mailbox).await? {
            return Ok(());
        }
        within(self.imap.create(utf7::encode(mailbox))).await
    }

    async fn exists(&mut self, mailbox: &str) -> Result<bool> {
        let listed = within(self.imap.list("", utf7::encode(mailbox))).await?;
        Ok(!listed.is_empty())
    }

    async fn folders(&mut self) -> Result<Vec<String>> {
        let listed = within(self.imap.list("", "*")).await?;
        Ok(listed
            .into_iter()
            .map(|(mailbox, _, _)| mailbox_name(&mailbox))
            .collect())
    }
}

impl Client {
    async fn store_keyword(
        &mut self,
        mailbox: &str,
        uids: &[u32],
        keyword: &str,
        kind: StoreType,
    ) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        self.select(mailbox).await?;
        let flag = keyword_flag(keyword)?;
        within(self.imap.uid_silent_store(uid_set(uids)?, kind, [flag])).await
    }

    /// Move for servers without MOVE. The source is only expunged once every
    /// message is in `dest`, found there already or copied now, and only
    /// those messages are expunged.
    async fn copy_delete(&mut self, mailbox: &str, uids: &[u32], dest: &str) -> Result<()> {
        let held = self.identify(mailbox, uids).await?;
        // None left: an earlier attempt finished the move.
        if held.is_empty() {
            return Ok(());
        }

        let mut missing = Vec::new();
        for message in &held {
            if !self.has_copy(dest, message).await? {
                missing.push(message.uid);
            }
        }

        self.select(mailbox).await?;
        if !missing.is_empty() {
            let set = uid_set(&missing)?;
            within(self.imap.uid_copy(set, utf7::encode(dest))).await?;
        }

        let gone: Vec<u32> = held.iter().map(|m| m.uid).collect();
        self.delete_and_expunge(&gone).await
    }

    /// Flags the messages `\Deleted` and expunges exactly them.
    async fn delete_and_expunge(&mut self, uids: &[u32]) -> Result<()> {
        let set = uid_set(uids)?;
        within(
            self.imap
                .uid_silent_store(set.clone(), StoreType::Add, [Flag::Deleted]),
        )
        .await?;
        let outcome = within(async { self.imap.resolve(UidExpungeTask::new(set)).await }).await?;
        outcome.map_err(|err| Error::Server(err.to_string()))
    }

    /// What tells the given messages apart. UIDs the folder no longer holds
    /// are left out.
    async fn identify(&mut self, mailbox: &str, uids: &[u32]) -> Result<Vec<Held>> {
        let items = vec![
            MessageDataItemName::Uid,
            MessageDataItemName::Envelope,
            MessageDataItemName::Rfc822Size,
        ];
        let fetched = self.fetch_items(mailbox, uids, items).await?;

        let mut held: Vec<Held> = fetched
            .into_iter()
            .map(|(uid, items)| {
                let mut message = Held {
                    uid: uid.get(),
                    message_id: String::new(),
                    size: 0,
                };
                for item in items.as_ref() {
                    match item {
                        MessageDataItem::Rfc822Size(size) => message.size = *size,
                        MessageDataItem::Envelope(envelope) => {
                            message.message_id = bracket(&text(&envelope.message_id));
                        }
                        _ => {}
                    }
                }
                message
            })
            .collect();
        held.sort_by_key(|h| h.uid);
        Ok(held)
    }

    /// Whether `dest` holds a message with the same Message-ID and size. COPY
    /// keeps a message's bytes, so a different size means a different message
    /// that reuses an ID, which must not be taken for the copy. A message that
    /// can't be told apart this way counts as not copied.
    async fn has_copy(&mut self, dest: &str, held: &Held) -> Result<bool> {
        if held.message_id.is_empty() || held.size == 0 {
            return Ok(false);
        }
        // LARGER and SMALLER are exclusive bounds.
        let criteria = Vec1::try_from(vec![
            SearchKey::Header(
                AString::try_from("Message-ID".to_owned()).map_err(invalid)?,
                AString::try_from(held.message_id.clone()).map_err(invalid)?,
            ),
            SearchKey::Larger(held.size.saturating_sub(1)),
            SearchKey::Smaller(held.size.saturating_add(1)),
        ])
        .map_err(invalid)?;
        Ok(!self.uid_search(dest, criteria).await?.is_empty())
    }
}

/// A message as far as telling it apart from its copy: the same Message-ID and
/// size mean the same message.
struct Held {
    uid: u32,
    message_id: String,
    size: u32,
}

#[cfg(test)]
mod tests {
    use imap_client::imap_types::response::{StatusBody, StatusKind};

    use super::*;

    fn refused(text: &'static str) -> ClientError {
        ClientError::ResolveTask(TaskError::UnexpectedNoResponse(StatusBody {
            kind: StatusKind::No,
            code: None,
            text: text.try_into().unwrap(),
        }))
    }

    #[test]
    fn a_rejected_password_is_a_login_error() {
        let err = login_error(refused("[AUTHENTICATIONFAILED] Authentication failed."));

        assert!(matches!(err, Error::Login(_)), "{err:?}");
    }

    #[test]
    fn a_connection_limit_is_not_a_login_error() {
        for text in [
            "[ALERT] Too many simultaneous connections. (Failure)",
            "[LIMIT] Maximum number of connections from user+IP exceeded (mail_max_userip_connections)",
        ] {
            let err = login_error(refused(text));

            assert!(matches!(err, Error::Server(_)), "{text}: {err:?}");
        }
    }
}
