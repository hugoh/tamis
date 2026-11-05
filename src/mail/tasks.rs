//! IMAP commands `imap-client` doesn't offer: `STATUS`, and `UID EXPUNGE`
//! (its own expunge is a bare `EXPUNGE`, which could purge other messages
//! already marked `\Deleted`).

use std::num::NonZeroU32;

use imap_client::imap_types::command::CommandBody;
use imap_client::imap_types::mailbox::Mailbox;
use imap_client::imap_types::response::{Data, StatusBody, StatusKind};
use imap_client::imap_types::sequence::SequenceSet;
use imap_client::imap_types::status::{StatusDataItem, StatusDataItemName};
use imap_client::tasks::Task;
use imap_client::tasks::tasks::TaskError;

/// The values a `STATUS` returned.
#[derive(Clone, Debug, Default)]
pub struct StatusValues {
    pub messages: Option<u32>,
    pub uid_next: Option<NonZeroU32>,
    pub uid_validity: Option<NonZeroU32>,
    pub highest_mod_seq: Option<u64>,
}

pub struct StatusTask {
    mailbox: Mailbox<'static>,
    items: Vec<StatusDataItemName>,
    values: StatusValues,
}

impl StatusTask {
    pub fn new(mailbox: Mailbox<'static>, items: Vec<StatusDataItemName>) -> Self {
        Self {
            mailbox,
            items,
            values: StatusValues::default(),
        }
    }
}

fn finish<T>(status: StatusBody<'static>, ok: T) -> Result<T, TaskError> {
    match status.kind {
        StatusKind::Ok => Ok(ok),
        StatusKind::No => Err(TaskError::UnexpectedNoResponse(status)),
        StatusKind::Bad => Err(TaskError::UnexpectedBadResponse(status)),
    }
}

impl Task for StatusTask {
    type Output = Result<StatusValues, TaskError>;

    fn command_body(&self) -> CommandBody<'static> {
        CommandBody::Status {
            mailbox: self.mailbox.clone(),
            item_names: self.items.clone().into(),
        }
    }

    fn process_data(&mut self, data: Data<'static>) -> Option<Data<'static>> {
        let Data::Status { items, .. } = data else {
            return Some(data);
        };
        for item in items.iter() {
            match item {
                StatusDataItem::Messages(n) => self.values.messages = Some(*n),
                StatusDataItem::UidNext(n) => self.values.uid_next = Some(*n),
                StatusDataItem::UidValidity(n) => self.values.uid_validity = Some(*n),
                StatusDataItem::HighestModSeq(n) => self.values.highest_mod_seq = Some(*n),
                _ => {}
            }
        }
        None
    }

    fn process_tagged(self, status_body: StatusBody<'static>) -> Self::Output {
        finish(status_body, self.values)
    }
}

/// `UID EXPUNGE`: removes only the given messages, and only if they carry
/// `\Deleted`.
pub struct UidExpungeTask {
    uids: SequenceSet,
}

impl UidExpungeTask {
    pub fn new(uids: SequenceSet) -> Self {
        Self { uids }
    }
}

impl Task for UidExpungeTask {
    type Output = Result<(), TaskError>;

    fn command_body(&self) -> CommandBody<'static> {
        CommandBody::ExpungeUid {
            sequence_set: self.uids.clone(),
        }
    }

    fn process_data(&mut self, data: Data<'static>) -> Option<Data<'static>> {
        match data {
            Data::Expunge(_) => None,
            other => Some(other),
        }
    }

    fn process_tagged(self, status_body: StatusBody<'static>) -> Self::Output {
        finish(status_body, ())
    }
}
