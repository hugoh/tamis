use thiserror::Error;

use crate::{mail, store};

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Mail(#[from] mail::Error),
    #[error(transparent)]
    Store(#[from] store::Error),
    /// The message is no longer where tamis put it.
    #[error(
        "{message_id} in {folder}: message not found where tamis left it; was it moved or deleted?"
    )]
    MessageGone { message_id: String, folder: String },
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
            Self::Store(_) | Self::MessageGone { .. } => false,
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
