//! An in-memory mailbox that behaves like an IMAP server where the engine
//! cares: UIDs only grow, a move gives a message a new UID in the destination
//! and keeps its flags, keywords are case-insensitive, and Message-ID
//! searches match substrings.

#![allow(
    clippy::expect_used,
    reason = "the helpers panic when a test misuses them"
)]
#![allow(
    clippy::unused_async_trait_impl,
    reason = "the trait is async; the fake has nothing to await"
)]

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use jiff::Timestamp;
use mail_parser::{AddressList, MessageParser};
use tokio_util::sync::CancellationToken;

use super::{Error, Mailbox, Message, ModState, Result, State};

#[derive(Clone, Debug)]
struct Stored {
    uid: u32,
    flags: BTreeSet<String>,
    raw: Vec<u8>,
    received: Timestamp,
    mod_seq: u64,
}

#[derive(Clone, Debug)]
struct Folder {
    uid_validity: u32,
    uid_next: u32,
    mod_seq: u64,
    messages: Vec<Stored>,
}

/// An in-memory [`Mailbox`].
#[derive(Debug, Default)]
pub struct Fake {
    folders: BTreeMap<String, Folder>,
    /// Whether the server advertises CONDSTORE.
    pub condstore: bool,
    faults: HashMap<&'static str, VecDeque<Error>>,
    lost_replies: HashMap<&'static str, VecDeque<Error>>,
    cancels: HashMap<&'static str, CancellationToken>,
    /// Every operation performed, as "operation folder", oldest first.
    pub log: Vec<String>,
}

impl Fake {
    #[must_use]
    pub fn new(folders: &[&str]) -> Self {
        let mut fake = Self::default();
        for name in folders {
            fake.create(name);
        }
        fake
    }

    /// Adds an empty folder.
    pub fn add_folder(&mut self, name: &str) {
        self.create(name);
    }

    /// Deletes a folder and its messages.
    pub fn remove_folder(&mut self, name: &str) {
        self.folders.remove(name);
    }

    fn create(&mut self, name: &str) {
        self.folders.entry(name.to_owned()).or_insert(Folder {
            uid_validity: 1,
            uid_next: 1,
            mod_seq: 1,
            messages: Vec::new(),
        });
    }

    /// Makes the next call to `operation` ("move", "`add_keyword`", ...) fail
    /// with `error`; calls queue up.
    pub fn fail(&mut self, operation: &'static str, error: Error) {
        self.faults.entry(operation).or_default().push_back(error);
    }

    /// Makes the next call to `operation` take effect and then fail with
    /// `error`, as when the connection drops before the reply arrives.
    pub fn fail_after(&mut self, operation: &'static str, error: Error) {
        self.lost_replies
            .entry(operation)
            .or_default()
            .push_back(error);
    }

    /// Cancels `token` when `operation` is next called, as a shutdown
    /// arriving mid-pass.
    pub fn cancel_on(&mut self, operation: &'static str, token: CancellationToken) {
        self.cancels.insert(operation, token);
    }

    fn reply(&mut self, operation: &'static str) -> Result<()> {
        match self
            .lost_replies
            .get_mut(operation)
            .and_then(VecDeque::pop_front)
        {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Delivers a message into `folder` and returns its UID.
    ///
    /// # Panics
    /// When the folder doesn't exist.
    pub fn deliver(&mut self, folder: &str, raw: &str, flags: &[&str]) -> u32 {
        self.deliver_at(folder, raw, flags, Timestamp::now())
    }

    /// [`Fake::deliver`] with a received time.
    ///
    /// # Panics
    /// When the folder doesn't exist.
    pub fn deliver_at(
        &mut self,
        folder: &str,
        raw: &str,
        flags: &[&str],
        received: Timestamp,
    ) -> u32 {
        let folder = self.folders.get_mut(folder).expect("the folder exists");
        let uid = folder.uid_next;
        folder.uid_next += 1;
        folder.mod_seq += 1;
        folder.messages.push(Stored {
            uid,
            flags: flags.iter().map(|f| f.to_lowercase()).collect(),
            raw: raw.as_bytes().to_vec(),
            received,
            mod_seq: folder.mod_seq,
        });
        uid
    }

    /// Changes the folder's `UIDVALIDITY`, as when it was recreated.
    ///
    /// # Panics
    /// When the folder doesn't exist.
    pub fn reset_uid_validity(&mut self, folder: &str, uid_validity: u32) {
        self.folders
            .get_mut(folder)
            .expect("the folder exists")
            .uid_validity = uid_validity;
    }

    /// The flags of a message, sorted.
    #[must_use]
    pub fn flags(&self, folder: &str, uid: u32) -> Option<Vec<String>> {
        let found = self
            .folders
            .get(folder)?
            .messages
            .iter()
            .find(|m| m.uid == uid)?;
        Some(found.flags.iter().cloned().collect())
    }

    /// The UIDs in a folder, ascending.
    #[must_use]
    pub fn uids(&self, folder: &str) -> Vec<u32> {
        self.folders
            .get(folder)
            .map(|f| f.messages.iter().map(|m| m.uid).collect())
            .unwrap_or_default()
    }

    /// The raw source of a message.
    #[must_use]
    pub fn raw(&self, folder: &str, uid: u32) -> Option<String> {
        let found = self
            .folders
            .get(folder)?
            .messages
            .iter()
            .find(|m| m.uid == uid)?;
        Some(String::from_utf8_lossy(&found.raw).into_owned())
    }

    fn check(&mut self, operation: &'static str, folder: &str) -> Result<()> {
        self.log.push(format!("{operation} {folder}"));
        if let Some(token) = self.cancels.remove(operation) {
            token.cancel();
        }
        match self.faults.get_mut(operation).and_then(VecDeque::pop_front) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn folder(&self, name: &str) -> Result<&Folder> {
        self.folders
            .get(name)
            .ok_or_else(|| Error::Server(format!("no such folder: {name}")))
    }

    fn folder_mut(&mut self, name: &str) -> Result<&mut Folder> {
        self.folders
            .get_mut(name)
            .ok_or_else(|| Error::Server(format!("no such folder: {name}")))
    }

    fn set_flag(&mut self, mailbox: &str, uids: &[u32], keyword: &str, on: bool) -> Result<()> {
        let keyword = keyword.to_lowercase();
        let folder = self.folder_mut(mailbox)?;
        let mut changed = false;
        for message in folder.messages.iter_mut().filter(|m| uids.contains(&m.uid)) {
            let did = if on {
                message.flags.insert(keyword.clone())
            } else {
                message.flags.remove(&keyword)
            };
            if did {
                changed = true;
                message.mod_seq = folder.mod_seq + 1;
            }
        }
        if changed {
            folder.mod_seq += 1;
        }
        Ok(())
    }
}

fn envelope(stored: &Stored, with_source: bool) -> Message {
    let parsed = MessageParser::default().parse_headers(&stored.raw);
    let from = parsed
        .as_ref()
        .and_then(|m| m.from()?.first()?.address().map(str::to_lowercase))
        .unwrap_or_default();
    let message_id = parsed
        .as_ref()
        .and_then(|m| m.message_id())
        .map(|id| {
            if id.starts_with('<') {
                id.to_owned()
            } else {
                format!("<{id}>")
            }
        })
        .unwrap_or_default();

    Message {
        uid: stored.uid,
        message_id,
        from,
        subject: parsed
            .as_ref()
            .and_then(|m| m.subject())
            .unwrap_or_default()
            .to_owned(),
        flags: stored.flags.iter().cloned().collect(),
        raw: if with_source {
            stored.raw.clone()
        } else {
            Vec::new()
        },
    }
}

fn addresses(address: Option<AddressList<'_>>) -> Vec<String> {
    address
        .into_iter()
        .flat_map(|list| list.mailboxes())
        .filter_map(|mailbox| mailbox.address())
        .map(str::to_lowercase)
        .collect()
}

impl Mailbox for Fake {
    async fn state(&mut self, mailbox: &str) -> Result<State> {
        self.check("state", mailbox)?;
        let folder = self.folder(mailbox)?;
        Ok(State {
            uid_validity: folder.uid_validity,
            uid_next: folder.uid_next,
        })
    }

    async fn messages(&mut self, mailbox: &str) -> Result<u32> {
        self.check("messages", mailbox)?;
        Ok(u32::try_from(self.folder(mailbox)?.messages.len()).unwrap_or(u32::MAX))
    }

    async fn uids_after(&mut self, mailbox: &str, after: u32) -> Result<Vec<u32>> {
        self.check("uids_after", mailbox)?;
        Ok(self
            .folder(mailbox)?
            .messages
            .iter()
            .map(|m| m.uid)
            .filter(|uid| *uid > after)
            .collect())
    }

    async fn seen_before(&mut self, mailbox: &str, before: Timestamp) -> Result<Vec<u32>> {
        self.check("seen_before", mailbox)?;
        Ok(self
            .folder(mailbox)?
            .messages
            .iter()
            .filter(|m| m.flags.contains("\\seen") && m.received < before)
            .map(|m| m.uid)
            .collect())
    }

    async fn find_message_id(&mut self, mailbox: &str, message_id: &str) -> Result<Vec<u32>> {
        self.check("find_message_id", mailbox)?;
        if message_id.is_empty() {
            return Ok(Vec::new());
        }
        let wanted = message_id.to_lowercase();
        Ok(self
            .folder(mailbox)?
            .messages
            .iter()
            .filter(|m| {
                envelope(m, false)
                    .message_id
                    .to_lowercase()
                    .contains(&wanted)
            })
            .map(|m| m.uid)
            .collect())
    }

    async fn mod_state(&mut self, mailbox: &str) -> Result<Option<ModState>> {
        self.check("mod_state", mailbox)?;
        let condstore = self.condstore;
        let folder = self.folder(mailbox)?;
        Ok(condstore.then_some(ModState {
            uid_validity: folder.uid_validity,
            highest_mod_seq: folder.mod_seq,
        }))
    }

    async fn search_keywords(
        &mut self,
        mailbox: &str,
        keywords: &[String],
        since: Timestamp,
        changed_since: u64,
    ) -> Result<Vec<u32>> {
        self.check("search_keywords", mailbox)?;
        let wanted: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();
        Ok(self
            .folder(mailbox)?
            .messages
            .iter()
            .filter(|m| m.received >= since && m.mod_seq > changed_since)
            .filter(|m| wanted.iter().any(|k| m.flags.contains(k)))
            .map(|m| m.uid)
            .collect())
    }

    async fn fetch(&mut self, mailbox: &str, uids: &[u32]) -> Result<Vec<Message>> {
        self.check("fetch", mailbox)?;
        let folder = self.folder(mailbox)?;
        Ok(folder
            .messages
            .iter()
            .filter(|m| uids.contains(&m.uid))
            .map(|m| envelope(m, true))
            .collect())
    }

    async fn fetch_envelopes(&mut self, mailbox: &str, uids: &[u32]) -> Result<Vec<Message>> {
        self.check("fetch_envelopes", mailbox)?;
        let folder = self.folder(mailbox)?;
        Ok(folder
            .messages
            .iter()
            .filter(|m| uids.contains(&m.uid))
            .map(|m| envelope(m, false))
            .collect())
    }

    async fn recipients(&mut self, mailbox: &str, uids: &[u32]) -> Result<Vec<String>> {
        self.check("recipients", mailbox)?;
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for stored in self
            .folder(mailbox)?
            .messages
            .iter()
            .filter(|m| uids.contains(&m.uid))
        {
            let Some(parsed) = MessageParser::default().parse_headers(&stored.raw) else {
                continue;
            };
            for address in addresses(parsed.to())
                .into_iter()
                .chain(addresses(parsed.cc()))
            {
                if !address.is_empty() && seen.insert(address.clone()) {
                    out.push(address);
                }
            }
        }
        Ok(out)
    }

    async fn move_messages(&mut self, mailbox: &str, uids: &[u32], dest: &str) -> Result<()> {
        self.check("move", mailbox)?;
        self.folder(dest)?;
        let source = self.folder_mut(mailbox)?;

        let (moving, staying): (Vec<_>, Vec<_>) = source
            .messages
            .drain(..)
            .partition(|m| uids.contains(&m.uid));
        source.messages = staying;
        source.mod_seq += 1;

        let dest = self.folder_mut(dest)?;
        for mut message in moving {
            message.uid = dest.uid_next;
            dest.uid_next += 1;
            dest.mod_seq += 1;
            message.mod_seq = dest.mod_seq;
            dest.messages.push(message);
        }
        self.reply("move")
    }

    async fn add_keyword(&mut self, mailbox: &str, uids: &[u32], keyword: &str) -> Result<()> {
        self.check("add_keyword", mailbox)?;
        self.set_flag(mailbox, uids, keyword, true)
    }

    async fn remove_keyword(&mut self, mailbox: &str, uids: &[u32], keyword: &str) -> Result<()> {
        self.check("remove_keyword", mailbox)?;
        self.set_flag(mailbox, uids, keyword, false)
    }

    async fn ensure(&mut self, mailbox: &str) -> Result<()> {
        self.check("ensure", mailbox)?;
        self.create(mailbox);
        Ok(())
    }

    async fn exists(&mut self, mailbox: &str) -> Result<bool> {
        self.check("exists", mailbox)?;
        Ok(self.folders.contains_key(mailbox))
    }

    async fn folders(&mut self) -> Result<Vec<String>> {
        self.check("folders", "")?;
        Ok(self.folders.keys().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIL: &str = "From: Alice <Alice@Example.org>\r\nTo: me@example.org, Bob <bob@example.org>\r\nCc: me@example.org\r\nSubject: Hi\r\nMessage-ID: <abc@x>\r\n\r\nbody";

    #[tokio::test]
    async fn moving_assigns_new_uids_and_keeps_flags() {
        let mut fake = Fake::new(&["INBOX", "Junk"]);
        let uid = fake.deliver("INBOX", MAIL, &["$Junk"]);
        fake.deliver("Junk", MAIL, &[]);

        fake.move_messages("INBOX", &[uid], "Junk").await.unwrap();

        assert_eq!(fake.uids("INBOX"), Vec::<u32>::new());
        assert_eq!(fake.uids("Junk"), [1, 2]);
        assert_eq!(fake.flags("Junk", 2), Some(vec!["$junk".to_owned()]));
        assert_eq!(fake.state("Junk").await.unwrap().uid_next, 3);
        fake.move_messages("INBOX", &[uid], "Junk").await.unwrap();
        assert_eq!(
            fake.uids("Junk"),
            [1, 2],
            "moving what is gone does nothing"
        );
    }

    #[tokio::test]
    async fn envelopes_and_searches_read_the_headers() {
        let mut fake = Fake::new(&["INBOX"]);
        let uid = fake.deliver("INBOX", MAIL, &[]);

        let message = &fake.fetch("INBOX", &[uid]).await.unwrap()[0];
        assert_eq!(
            (
                message.from.as_str(),
                message.message_id.as_str(),
                message.subject.as_str()
            ),
            ("alice@example.org", "<abc@x>", "Hi")
        );
        assert!(
            fake.fetch_envelopes("INBOX", &[uid]).await.unwrap()[0]
                .raw
                .is_empty()
        );

        assert_eq!(fake.find_message_id("INBOX", "abc@x").await.unwrap(), [uid]);
        assert!(fake.find_message_id("INBOX", "").await.unwrap().is_empty());
        assert_eq!(
            fake.recipients("INBOX", &[uid]).await.unwrap(),
            ["me@example.org", "bob@example.org"]
        );
    }

    #[tokio::test]
    async fn keywords_are_case_insensitive_and_tracked_by_mod_seq() {
        let mut fake = Fake::new(&["INBOX"]);
        fake.condstore = true;
        let uid = fake.deliver("INBOX", MAIL, &[]);
        let before = fake
            .mod_state("INBOX")
            .await
            .unwrap()
            .unwrap()
            .highest_mod_seq;

        fake.add_keyword("INBOX", &[uid], "$Junk").await.unwrap();
        let long_ago = Timestamp::UNIX_EPOCH;
        let keywords = vec!["$JUNK".to_owned()];
        assert_eq!(
            fake.search_keywords("INBOX", &keywords, long_ago, 0)
                .await
                .unwrap(),
            [uid]
        );
        assert_eq!(
            fake.search_keywords("INBOX", &keywords, long_ago, before)
                .await
                .unwrap(),
            [uid]
        );
        let after = fake
            .mod_state("INBOX")
            .await
            .unwrap()
            .unwrap()
            .highest_mod_seq;
        assert!(
            fake.search_keywords("INBOX", &keywords, long_ago, after)
                .await
                .unwrap()
                .is_empty()
        );

        fake.remove_keyword("INBOX", &[uid], "$junk").await.unwrap();
        assert!(
            fake.search_keywords("INBOX", &keywords, long_ago, 0)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn faults_fire_once_and_folders_can_be_created() {
        let mut fake = Fake::new(&["INBOX"]);
        fake.fail("move", Error::Connection("gone".into()));

        assert!(
            fake.move_messages("INBOX", &[1], "INBOX")
                .await
                .unwrap_err()
                .is_connection()
        );
        assert!(fake.move_messages("INBOX", &[1], "INBOX").await.is_ok());

        assert!(!fake.exists("Lists").await.unwrap());
        fake.ensure("Lists").await.unwrap();
        assert_eq!(fake.folders().await.unwrap(), ["INBOX", "Lists"]);
        assert!(matches!(fake.state("Nope").await, Err(Error::Server(_))));
    }

    #[tokio::test]
    async fn seen_mail_before_a_time() {
        let mut fake = Fake::new(&["INBOX"]);
        let old = Timestamp::from_second(1_000).unwrap();
        let read = fake.deliver_at("INBOX", MAIL, &["\\Seen"], old);
        fake.deliver_at("INBOX", MAIL, &[], old);
        fake.deliver_at(
            "INBOX",
            MAIL,
            &["\\Seen"],
            Timestamp::from_second(9_000).unwrap(),
        );

        let before = Timestamp::from_second(5_000).unwrap();
        assert_eq!(fake.seen_before("INBOX", before).await.unwrap(), [read]);
        assert_eq!(fake.messages("INBOX").await.unwrap(), 3);
        assert_eq!(fake.uids_after("INBOX", 1).await.unwrap(), [2, 3]);
    }
}
