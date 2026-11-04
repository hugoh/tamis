pub use crate::journal::{Action, Reason, Verdict};

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
