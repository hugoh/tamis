//! Scores mail as spam or good with a personal token Bayes model
//! (Robinson/Fisher chi-square combining, as in `SpamBayes`).

mod bayes;
mod recipients;
mod tokenize;

pub use bayes::{Clue, Label, Model, Score, score};
pub use recipients::{recipient_status_tokens, with_recipient_status};
pub use tokenize::tokenize;

/// Which tokenizer the token counts were learnt with. Raise it whenever a
/// change to `tokenize` alters the tokens of existing mail, so that counts
/// learnt earlier are reported as stale until `tamis bootstrap --refresh`
/// learns them again.
///
/// 1 is the tokenizer from before versions were recorded; 2 splits words by
/// the Unicode word rules and pairs the characters of Chinese, Japanese and
/// Thai.
pub const TOKENIZER_VERSION: u32 = 2;

/// The version of counts learnt before it was recorded.
pub const FIRST_TOKENIZER_VERSION: u32 = 1;
