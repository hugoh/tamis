use std::fmt;

/// Keywords tamis sets on messages it acts on.
pub mod keyword {
    /// Marks mail tamis moved to Junk. Seeing it outside Junk later means the
    /// user disagreed.
    pub const FILED: &str = "$TamisFiled";
    /// Marks mail tamis couldn't decide on.
    pub const UNCERTAIN: &str = "$TamisUncertain";
    /// Marks mail tamis moved out of Junk. Seeing it back in Junk means the
    /// user disagreed.
    pub const RESCUED: &str = "$TamisRescued";
    /// Marks mail tamis already learnt that it moved on to a folder it scans.
    /// Senders can't set keywords, unlike a Message-ID.
    pub const TRAINED: &str = "$TamisTrained";
    /// Marks mail tamis filed by a rule. Seeing it back in a watched folder
    /// means the user moved it there, so it isn't filed again, provided the
    /// journal shows tamis filed it: a move that failed leaves the keyword
    /// behind, and that message is filed on the next pass.
    pub const SORTED: &str = "$TamisSorted";
}

/// What tamis concluded about a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Spam,
    Uncertain,
    Good,
}

impl Verdict {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spam => "spam",
            Self::Uncertain => "uncertain",
            Self::Good => "good",
        }
    }
}

/// Why tamis concluded it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Bayes,
    Whitelisted,
    Correction,
    Rule,
    TrainFolder,
    ProviderJunk,
    Undo,
}

impl Reason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bayes => "bayes",
            Self::Whitelisted => "whitelisted",
            Self::Correction => "correction",
            Self::Rule => "rule",
            Self::TrainFolder => "train folder",
            Self::ProviderJunk => "provider junk",
            Self::Undo => "undo",
        }
    }
}

/// What tamis did, or in shadow mode would have done. The journal stores it
/// as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Move(String),
    Keyword(String),
    TrainGood,
    TrainSpam,
    /// A keyword was cleared by an undo.
    Unflag(String),
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::Move(folder) => write!(f, "move:{folder}"),
            Self::Keyword(keyword) => write!(f, "keyword:{keyword}"),
            Self::TrainGood => f.write_str("train:good"),
            Self::TrainSpam => f.write_str("train:spam"),
            Self::Unflag(keyword) => write!(f, "unflag:{keyword}"),
        }
    }
}

/// A verdict with its reason and the score behind it (zero when no score was
/// computed).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Judgement {
    pub verdict: Verdict,
    pub reason: Reason,
    pub score: f64,
}

/// Counts what one pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub scanned: usize,
    pub spam: usize,
    pub uncertain: usize,
    pub good: usize,
    pub corrected: usize,
    pub trained: usize,
    pub rescued: usize,
}

impl Summary {
    pub(super) fn count(&mut self, verdict: Verdict) {
        match verdict {
            Verdict::Spam => self.spam += 1,
            Verdict::Uncertain => self.uncertain += 1,
            Verdict::Good => self.good += 1,
        }
    }
}
