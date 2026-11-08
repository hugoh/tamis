use thiserror::Error;

use crate::{mail, store};

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Mail(#[from] mail::Error),
    #[error(transparent)]
    Store(#[from] store::Error),
    #[error(transparent)]
    Rules(#[from] crate::rules::Error),
    /// The message is no longer where tamis put it.
    #[error(
        "{message_id} in {folder}: message not found where tamis left it; was it moved or deleted?"
    )]
    MessageGone { message_id: String, folder: String },
    /// The decision was only recorded (shadow mode, or a verdict with no action).
    #[error("nothing to undo: the action was never applied")]
    NothingToUndo,
    /// The decision isn't one of tamis's own filings.
    #[error(
        "only tamis's own filing can be undone; to retrain, drop the message in the other training folder"
    )]
    NotUndoable,
    /// Tamis acted on the decision, so undo is the way to change it.
    #[error("the action was applied; use undo to reverse it")]
    AlreadyApplied,
    /// The decision isn't one of tamis's own judgements.
    #[error(
        "only tamis's own judgements can be learnt from; to retrain, drop the message in a training folder"
    )]
    NotLearnable,
    /// The decision has a later correction; ask for a label to change it again.
    #[error("already corrected by decision {0}; pass --good or --spam to change it")]
    AlreadyCorrected(i64),
    /// An uncertain verdict has no opposite.
    #[error("an uncertain verdict needs an explicit label: good or spam")]
    NeedsLabel,
    /// The pass ended early because tamis is shutting down.
    #[error("stopped for shutdown")]
    Stopped,
    /// An error with what was being done when it happened.
    #[error("{context}: {source}")]
    Context { context: String, source: Box<Error> },
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Whether the connection itself failed, as when the server hangs up
    /// mid-command: nothing more can be done on it.
    #[must_use]
    pub fn is_connection(&self) -> bool {
        match self {
            Self::Mail(err) => err.is_connection(),
            Self::Context { source, .. } => source.is_connection(),
            _ => false,
        }
    }

    /// Whether the server answered that a command failed, which says nothing
    /// about the connection or about any other message.
    #[must_use]
    pub fn is_server_refusal(&self) -> bool {
        match self {
            Self::Mail(mail::Error::Server(_)) => true,
            Self::Context { source, .. } => source.is_server_refusal(),
            _ => false,
        }
    }

    /// Whether the server rejected the credentials.
    #[must_use]
    pub fn is_login_rejected(&self) -> bool {
        match self {
            Self::Mail(mail::Error::Login(_)) => true,
            Self::Context { source, .. } => source.is_login_rejected(),
            _ => false,
        }
    }

    /// Whether the pass ended early because tamis is shutting down.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        match self {
            Self::Stopped => true,
            Self::Context { source, .. } => source.is_stopped(),
            _ => false,
        }
    }

    /// Whether a message wasn't where it was looked for.
    #[must_use]
    pub fn is_message_gone(&self) -> bool {
        match self {
            Self::MessageGone { .. } => true,
            Self::Context { source, .. } => source.is_message_gone(),
            _ => false,
        }
    }
}

/// Adds what was being done to an error.
pub trait Context<T> {
    /// # Errors
    /// The original error, wrapped with `context`.
    fn context(self, context: impl Into<String>) -> Result<T>;
}

impl<T, E: Into<Error>> Context<T> for std::result::Result<T, E> {
    fn context(self, context: impl Into<String>) -> Result<T> {
        self.map_err(|err| Error::Context {
            context: context.into(),
            source: Box::new(err.into()),
        })
    }
}
