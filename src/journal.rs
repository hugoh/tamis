//! The words the journal records: what tamis concluded about a message, why,
//! and what it did. They are stored as text, in the form the Go build wrote,
//! so a database moves between the two.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Text that isn't one of the journal's words.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("unknown {what} {text:?}")]
pub struct UnknownWord {
    what: &'static str,
    text: String,
}

/// What tamis concluded about a message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Verdict {
    Spam,
    Uncertain,
    #[default]
    Good,
}

impl Verdict {
    pub const ALL: [Self; 3] = [Self::Spam, Self::Uncertain, Self::Good];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spam => "spam",
            Self::Uncertain => "uncertain",
            Self::Good => "good",
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Verdict {
    type Err = UnknownWord;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|verdict| verdict.as_str() == text)
            .ok_or_else(|| UnknownWord {
                what: "verdict",
                text: text.to_owned(),
            })
    }
}

/// Why tamis concluded it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reason {
    #[default]
    Bayes,
    Whitelisted,
    Correction,
    Rule,
    TrainFolder,
    ProviderJunk,
    Undo,
    /// A message you sent: its recipients were whitelisted. Not a judgement of
    /// the message, so it can't be undone or learnt from.
    Sent,
}

impl Reason {
    pub const ALL: [Self; 8] = [
        Self::Bayes,
        Self::Whitelisted,
        Self::Correction,
        Self::Rule,
        Self::TrainFolder,
        Self::ProviderJunk,
        Self::Undo,
        Self::Sent,
    ];

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
            Self::Sent => "sent",
        }
    }

    /// Whether the verdict is tamis's own call, as opposed to learning from
    /// what the user did.
    #[must_use]
    pub fn is_own_judgement(self) -> bool {
        matches!(self, Self::Bayes | Self::Whitelisted)
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Reason {
    type Err = UnknownWord;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.as_str() == text)
            .ok_or_else(|| UnknownWord {
                what: "reason",
                text: text.to_owned(),
            })
    }
}

/// What tamis did, or in shadow mode would have done.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Action {
    #[default]
    None,
    Move(String),
    Keyword(String),
    TrainGood,
    TrainSpam,
    /// A keyword was cleared by an undo.
    Unflag(String),
    /// Something a newer or older build wrote that this one doesn't know.
    Other(String),
}

impl Action {
    /// Whether the action changes the mailbox, as opposed to the filter.
    #[must_use]
    pub fn is_none(&self) -> bool {
        *self == Self::None
    }
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
            Self::Other(text) => f.write_str(text),
        }
    }
}

impl FromStr for Action {
    type Err = std::convert::Infallible;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Ok(match text.split_once(':') {
            None if text == "none" => Self::None,
            Some(("move", folder)) => Self::Move(folder.to_owned()),
            Some(("keyword", keyword)) => Self::Keyword(keyword.to_owned()),
            Some(("train", "good")) => Self::TrainGood,
            Some(("train", "spam")) => Self::TrainSpam,
            Some(("unflag", keyword)) => Self::Unflag(keyword.to_owned()),
            _ => Self::Other(text.to_owned()),
        })
    }
}

/// Compares a word with its journal text, as the web and CLI show it.
macro_rules! equals_text {
    ($($word:ty),*) => {$(
        impl PartialEq<&str> for $word {
            #[allow(clippy::cmp_owned, reason = "Action has no borrowed text form")]
            fn eq(&self, text: &&str) -> bool {
                self.to_string() == *text
            }
        }
    )*};
}

equals_text!(Verdict, Reason, Action);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_word_round_trips_through_its_text() {
        for verdict in Verdict::ALL {
            assert_eq!(verdict.to_string().parse(), Ok(verdict));
        }
        for reason in Reason::ALL {
            assert_eq!(reason.to_string().parse(), Ok(reason));
        }
        for action in [
            Action::None,
            Action::Move("Junk".into()),
            Action::Move("a:b/c d".into()),
            Action::Keyword("$TamisUncertain".into()),
            Action::TrainGood,
            Action::TrainSpam,
            Action::Unflag("$TamisFiled".into()),
        ] {
            assert_eq!(action.to_string().parse(), Ok(action));
        }
    }

    #[test]
    fn the_go_build_wrote_these_words() {
        assert_eq!("provider junk".parse(), Ok(Reason::ProviderJunk));
        assert_eq!("train folder".parse(), Ok(Reason::TrainFolder));
        assert_eq!("move:Junk".parse(), Ok(Action::Move("Junk".into())));
    }

    #[test]
    fn words_this_build_does_not_know_are_refused_or_kept() {
        assert_eq!(
            "maybe".parse::<Verdict>().unwrap_err().to_string(),
            "unknown verdict \"maybe\""
        );
        assert!("Bayes".parse::<Reason>().is_err());
        assert_eq!(
            "quarantine:X".parse(),
            Ok(Action::Other("quarantine:X".into()))
        );
        assert_eq!(
            "quarantine:X".parse::<Action>().unwrap().to_string(),
            "quarantine:X"
        );
    }

    #[test]
    fn only_bayes_and_whitelisting_are_own_judgements() {
        let own: Vec<Reason> = Reason::ALL
            .into_iter()
            .filter(|r| r.is_own_judgement())
            .collect();

        assert_eq!(own, [Reason::Bayes, Reason::Whitelisted]);
    }
}
