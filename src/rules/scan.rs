//! A small scan of a Sieve script for its `fileinto` commands: the folders
//! they name, and whether each is a `:copy`. The compiled script can't be
//! inspected, and the folders must be known before any message arrives.

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    Tag(String),
    Str(String),
    Punct(char),
}

/// A `fileinto` whose folder is a literal.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct FileInto {
    pub folder: String,
    pub copy: bool,
}

/// Every `fileinto` command in `source`. Folder names built from a variable
/// (`${...}`) can't be known before the script runs and are left out.
pub(super) fn file_intos(source: &str) -> Vec<FileInto> {
    let tokens = tokenize(source);
    let mut found = Vec::new();
    let mut at = 0;

    while at < tokens.len() {
        at += 1;
        if !matches!(&tokens[at - 1], Token::Word(word) if word.eq_ignore_ascii_case("fileinto")) {
            continue;
        }

        let mut copy = false;
        while let Some(token) = tokens.get(at) {
            match token {
                Token::Tag(tag) if tag.eq_ignore_ascii_case("copy") => copy = true,
                Token::Tag(tag)
                    if ["flags", "specialuse", "mailboxid"]
                        .contains(&tag.to_lowercase().as_str()) =>
                {
                    at = skip_argument(&tokens, at + 1) - 1;
                }
                Token::Tag(_) => {}
                Token::Str(folder) => {
                    if !folder.contains("${") {
                        found.push(FileInto {
                            folder: folder.clone(),
                            copy,
                        });
                    }
                    break;
                }
                _ => break,
            }
            at += 1;
        }
    }
    found
}

/// Commands tamis can't honour, since it only moves mail over IMAP.
const UNSUPPORTED: &[&str] = &[
    "addflag",
    "addheader",
    "deleteheader",
    "discard",
    "ereject",
    "keep",
    "notify",
    "redirect",
    "reject",
    "removeflag",
    "setflag",
    "vacation",
];

/// The sorted, distinct commands in `source` that tamis ignores or denies,
/// plus `fileinto :copy` when the script uses it.
pub(super) fn unsupported(source: &str) -> Vec<String> {
    let tokens = tokenize(source);
    let mut found: Vec<String> = tokens
        .iter()
        .filter_map(|token| match token {
            Token::Word(word) => UNSUPPORTED
                .iter()
                .find(|name| word.eq_ignore_ascii_case(name))
                .map(|name| (*name).to_owned()),
            _ => None,
        })
        .collect();
    if file_intos(source).iter().any(|f| f.copy) {
        found.push("fileinto :copy".to_owned());
    }
    found.sort_unstable();
    found.dedup();
    found
}

/// The index after the argument starting at `at`: one string, or a bracketed
/// list of them.
fn skip_argument(tokens: &[Token], at: usize) -> usize {
    if tokens.get(at) != Some(&Token::Punct('[')) {
        return at + 1;
    }
    tokens[at..]
        .iter()
        .position(|t| *t == Token::Punct(']'))
        .map_or(tokens.len(), |end| at + end + 1)
}

fn tokenize(source: &str) -> Vec<Token> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut at = 0;

    while at < chars.len() {
        let ch = chars[at];
        match ch {
            '#' => at = skip_to(&chars, at, |c| c == '\n'),
            '/' if chars.get(at + 1) == Some(&'*') => {
                at = find(&chars, at + 2, &['*', '/']).map_or(chars.len(), |end| end + 2);
            }
            '"' => {
                let (text, next) = quoted(&chars, at + 1);
                tokens.push(Token::Str(text));
                at = next;
            }
            ':' => {
                let end = word_end(&chars, at + 1);
                tokens.push(Token::Tag(chars[at + 1..end].iter().collect()));
                at = end.max(at + 1);
            }
            c if c.is_alphabetic() || c == '_' => {
                let end = word_end(&chars, at);
                let word: String = chars[at..end].iter().collect();
                at = end;
                if word.eq_ignore_ascii_case("text") && chars.get(at) == Some(&':') {
                    let (text, next) = multiline(&chars, at + 1);
                    tokens.push(Token::Str(text));
                    at = next;
                } else {
                    tokens.push(Token::Word(word));
                }
            }
            c if c.is_whitespace() => at += 1,
            c => {
                tokens.push(Token::Punct(c));
                at += 1;
            }
        }
    }
    tokens
}

fn word_end(chars: &[char], from: usize) -> usize {
    chars[from.min(chars.len())..]
        .iter()
        .position(|c| !(c.is_alphanumeric() || *c == '_' || *c == '-'))
        .map_or(chars.len(), |len| from + len)
}

fn skip_to(chars: &[char], from: usize, stop: impl Fn(char) -> bool) -> usize {
    chars[from..]
        .iter()
        .position(|c| stop(*c))
        .map_or(chars.len(), |len| from + len)
}

fn find(chars: &[char], from: usize, needle: &[char]) -> Option<usize> {
    chars
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|len| from + len)
}

/// A quoted string starting after its opening quote; returns the text and
/// the index after the closing quote.
fn quoted(chars: &[char], from: usize) -> (String, usize) {
    let mut text = String::new();
    let mut at = from;
    while at < chars.len() {
        match chars[at] {
            '"' => return (text, at + 1),
            '\\' if at + 1 < chars.len() => {
                text.push(chars[at + 1]);
                at += 2;
            }
            c => {
                text.push(c);
                at += 1;
            }
        }
    }
    (text, at)
}

/// A `text:` block, after the colon: the rest of the line, then lines up to
/// one holding just ".".
fn multiline(chars: &[char], from: usize) -> (String, usize) {
    let body_start = skip_to(chars, from, |c| c == '\n') + 1;
    let rest: String = chars[body_start.min(chars.len())..].iter().collect();

    let mut text = String::new();
    let mut consumed = 0;
    for line in rest.split_inclusive('\n') {
        consumed += line.chars().count();
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "." {
            break;
        }
        text.push_str(
            trimmed
                .strip_prefix('.')
                .filter(|_| trimmed.starts_with(".."))
                .unwrap_or(trimmed),
        );
        text.push('\n');
    }
    (text, body_start + consumed)
}
