//! What tamis needs from an IMAP connection: the handful of mailbox
//! operations the engine uses, with the safety rules from the design built in
//! (fetches never mark mail as read; moves never fall back to a bare
//! `EXPUNGE`). [`fake::Fake`] is an in-memory implementation for tests.

pub mod client;
mod convert;
pub mod fake;
mod tasks;
pub mod utf7;
pub mod watch;

use jiff::Timestamp;
use thiserror::Error;

/// How much of each message is downloaded: enough for headers and the text
/// that classification looks at.
pub const MAX_MESSAGE_BYTES: usize = 256 << 10;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum Error {
    /// The connection itself failed, as when the server hangs up mid-command.
    /// Nothing more can be done on such a connection.
    #[error("connection: {0}")]
    Connection(String),
    /// The server answered that a command failed.
    #[error("{0}")]
    Server(String),
    /// The server rejected the credentials.
    #[error("login failed: {0}")]
    Login(String),
    /// The server has neither MOVE nor UIDPLUS, so the only way to move would
    /// be an `EXPUNGE` that could purge other messages already marked
    /// `\Deleted`.
    #[error("server supports neither MOVE nor UIDPLUS; refusing an unsafe move")]
    UnsafeMove,
    #[error("server does not support IDLE")]
    NoIdle,
}

impl Error {
    /// Whether the connection failed, rather than the server answering that a
    /// command failed.
    #[must_use]
    pub fn is_connection(&self) -> bool {
        matches!(self, Self::Connection(_))
    }
}

/// A folder's `UIDVALIDITY` and next UID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub uid_validity: u32,
    pub uid_next: u32,
}

/// A folder's CONDSTORE position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModState {
    pub uid_validity: u32,
    pub highest_mod_seq: u64,
}

/// A fetched message: the first [`MAX_MESSAGE_BYTES`] of its source and the
/// envelope fields tamis records.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Message {
    pub uid: u32,
    pub message_id: String,
    pub from: String,
    pub subject: String,
    /// Lower-cased: IMAP flags and keywords are case-insensitive.
    pub flags: Vec<String>,
    pub raw: Vec<u8>,
}

impl Message {
    /// Whether the message carries `keyword`, in any case.
    #[must_use]
    pub fn has_flag(&self, keyword: &str) -> bool {
        let keyword = keyword.to_lowercase();
        self.flags.contains(&keyword)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A logged-in IMAP connection, as far as the engine is concerned. Not safe
/// for concurrent use.
#[trait_variant::make(Mailbox: Send)]
pub trait LocalMailbox {
    /// The folder's current `UIDVALIDITY` and `UIDNEXT`.
    async fn state(&mut self, mailbox: &str) -> Result<State>;

    /// How many messages the folder holds, which a `STATUS` answers without
    /// selecting it.
    async fn messages(&mut self, mailbox: &str) -> Result<u32>;

    /// The UIDs in `mailbox` greater than `after`, ascending.
    async fn uids_after(&mut self, mailbox: &str, after: u32) -> Result<Vec<u32>>;

    /// Read messages that arrived before `before`.
    async fn seen_before(&mut self, mailbox: &str, before: Timestamp) -> Result<Vec<u32>>;

    /// The UIDs in `mailbox` whose Message-ID header contains `message_id`. An
    /// empty ID matches nothing.
    async fn find_message_id(&mut self, mailbox: &str, message_id: &str) -> Result<Vec<u32>>;

    /// The folder's `HIGHESTMODSEQ`, or `None` when the server doesn't
    /// advertise CONDSTORE.
    async fn mod_state(&mut self, mailbox: &str) -> Result<Option<ModState>>;

    /// Messages carrying any of `keywords`, received since `since`, whose
    /// flags changed after `changed_since` (zero: any).
    async fn search_keywords(
        &mut self,
        mailbox: &str,
        keywords: &[String],
        since: Timestamp,
        changed_since: u64,
    ) -> Result<Vec<u32>>;

    /// Downloads the given messages without marking them as read.
    async fn fetch(&mut self, mailbox: &str, uids: &[u32]) -> Result<Vec<Message>>;

    /// [`Mailbox::fetch`] without the message source: UID, flags and envelope
    /// fields only.
    async fn fetch_envelopes(&mut self, mailbox: &str, uids: &[u32]) -> Result<Vec<Message>>;

    /// The distinct To and Cc addresses of the given messages, lower-cased.
    async fn recipients(&mut self, mailbox: &str, uids: &[u32]) -> Result<Vec<String>>;

    /// Moves messages to `dest`. Can be repeated after a failure at any
    /// point: UIDs that are already gone count as moved.
    async fn move_messages(&mut self, mailbox: &str, uids: &[u32], dest: &str) -> Result<()>;

    /// Sets a flag or keyword on the given messages.
    async fn add_keyword(&mut self, mailbox: &str, uids: &[u32], keyword: &str) -> Result<()>;

    /// Clears a flag or keyword from the given messages.
    async fn remove_keyword(&mut self, mailbox: &str, uids: &[u32], keyword: &str) -> Result<()>;

    /// Creates `mailbox` unless it already exists.
    async fn ensure(&mut self, mailbox: &str) -> Result<()>;

    /// Whether `mailbox` is there.
    async fn exists(&mut self, mailbox: &str) -> Result<bool>;

    /// Every folder.
    async fn folders(&mut self) -> Result<Vec<String>>;
}
