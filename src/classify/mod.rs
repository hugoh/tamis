//! Scores mail as spam or good with a personal token Bayes model
//! (Robinson/Fisher chi-square combining, as in `SpamBayes`).

mod bayes;
mod recipients;
mod tokenize;

pub use bayes::{Clue, Label, Model, Score, score};
pub use recipients::{recipient_status_tokens, with_recipient_status};
pub use tokenize::tokenize;
