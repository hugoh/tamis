use std::cell::RefCell;
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::LazyLock;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, CharacterTokens, TagKind, TagToken, Token, TokenSink, TokenSinkResult, Tokenizer,
    TokenizerOpts,
};
use mail_parser::{AddressList, MessageParser, PartKind};
use regex::Regex;
use unicode_segmentation::UnicodeSegmentation as _;
use url::Url;

use super::recipients::{MAX_RECIPIENTS, RECIPIENT_PREFIX};

const MIN_WORD_LEN: usize = 3;
const MAX_WORD_LEN: usize = 12;
const MAX_PART_BYTES: usize = 64 << 10;
const SKIP_BUCKET: usize = 10;

// Only a link's host becomes a token, so the edges of its path don't matter. The
// `linkify` crate scored slightly worse in tamis-eval (see evaluation.md).
#[allow(
    clippy::expect_used,
    reason = "the pattern is a constant, checked by every test"
)]
static URL_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)https?://[^\s"'<>()]+"#).expect("the URL pattern is valid"));

/// Extracts the distinct features of a raw RFC 5322 message: words from the
/// subject and text parts, sender address and domain, recipient addresses and
/// domains, and link hosts. It never fails; unparsable input yields whatever
/// could be recovered.
#[must_use]
pub fn tokenize(raw: &[u8]) -> Vec<String> {
    let mut tokens = TokenSet::default();

    let Some(message) = MessageParser::default()
        .parse(raw)
        .filter(|m| !m.headers().is_empty())
    else {
        tokens.add_text("", &String::from_utf8_lossy(raw));
        return tokens.list;
    };

    if let Some(subject) = message.subject() {
        tokens.add_text("subject:", subject);
    }
    add_recipient_tokens(&mut tokens, message.to(), message.cc());
    for address in addresses(message.from()) {
        tokens.add(&format!("from:{address}"));
        if let Some((_, domain)) = address.split_once('@') {
            tokens.add(&format!("from-domain:{domain}"));
        }
    }

    for part in message.root().parts() {
        let (media, subtype) = part.content_type().map_or(("text", "plain"), |ct| {
            (ct.ctype(), ct.subtype().unwrap_or(""))
        });
        let is_html = media.eq_ignore_ascii_case("text") && subtype.eq_ignore_ascii_case("html");
        let is_plain = media.eq_ignore_ascii_case("text") && subtype.eq_ignore_ascii_case("plain");

        let wanted = match part.kind() {
            PartKind::Text => is_plain,
            PartKind::Html => is_html,
            _ => false,
        };
        let Some(body) = part.text().filter(|_| wanted) else {
            continue;
        };

        let body = truncate(&body, MAX_PART_BYTES);
        if is_html {
            tokens.add_text("", &strip_html(body));
        } else {
            tokens.add_text("", body);
        }
    }

    tokens.list
}

fn addresses(address: Option<AddressList<'_>>) -> Vec<String> {
    address
        .into_iter()
        .flat_map(|list| list.mailboxes())
        .filter_map(|mailbox| mailbox.address())
        .map(str::to_lowercase)
        .collect()
}

/// Records who the message went to: each distinct To and Cc address and its
/// domain, up to `MAX_RECIPIENTS` addresses.
fn add_recipient_tokens(
    tokens: &mut TokenSet,
    to: Option<AddressList<'_>>,
    cc: Option<AddressList<'_>>,
) {
    let mut seen = HashSet::new();

    for address in addresses(to).into_iter().chain(addresses(cc)) {
        if address.is_empty() || !seen.insert(address.clone()) {
            continue;
        }
        if seen.len() > MAX_RECIPIENTS {
            return;
        }

        tokens.add(&format!("{RECIPIENT_PREFIX}{address}"));
        if let Some((_, domain)) = address.split_once('@') {
            tokens.add(&format!("to-domain:{domain}"));
        }
    }
}

fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[derive(Default)]
struct TokenSet {
    seen: HashSet<String>,
    list: Vec<String>,
}

impl TokenSet {
    fn add(&mut self, token: &str) {
        if self.seen.insert(token.to_owned()) {
            self.list.push(token.to_owned());
        }
    }

    fn add_text(&mut self, prefix: &str, text: &str) {
        let mut rest = String::new();
        let mut last = 0;

        for span in URL_PATTERN.find_iter(text) {
            self.add_url(span.as_str());
            rest.push_str(&text[last..span.start()]);
            rest.push(' ');
            last = span.end();
        }
        rest.push_str(&text[last..]);

        self.add_words(prefix, &rest.to_lowercase());
    }

    /// Adds the words of `text` by the Unicode word-boundary rules (UAX #29).
    /// Chinese, Japanese hiragana and kanji, and Thai have no spaces between
    /// words, so each uninterrupted run of them is added as overlapping pairs
    /// instead.
    fn add_words(&mut self, prefix: &str, text: &str) {
        let mut run: Vec<&str> = Vec::new();
        let mut run_end = 0;

        for (start, word) in text.unicode_word_indices() {
            if word.chars().all(is_unspaced_script) {
                if start != run_end {
                    self.add_pairs(prefix, &mut run);
                }
                run.push(word);
                run_end = start + word.len();
                continue;
            }
            self.add_pairs(prefix, &mut run);
            self.add_word(prefix, word);
        }
        self.add_pairs(prefix, &mut run);
    }

    fn add_word(&mut self, prefix: &str, word: &str) {
        let length = word.chars().count();
        if length < MIN_WORD_LEN {
            return;
        }
        if length > MAX_WORD_LEN {
            let first = word.chars().next().unwrap_or_default();
            self.add(&format!(
                "skip:{first} {}",
                length / SKIP_BUCKET * SKIP_BUCKET
            ));
        } else {
            self.add(&format!("{prefix}{word}"));
        }
    }

    /// Adds each pair of neighbours in `run`, or the lone unit of a run of
    /// one, and empties the run.
    fn add_pairs(&mut self, prefix: &str, run: &mut Vec<&str>) {
        if let [only] = run.as_slice() {
            self.add(&format!("{prefix}{only}"));
        }
        for pair in run.windows(2) {
            self.add(&format!("{prefix}{}{}", pair[0], pair[1]));
        }
        run.clear();
    }

    /// Records the link's host and each parent domain down to the registrable
    /// one, so "a.b.example.co.uk" also counts as "example.co.uk".
    fn add_url(&mut self, link: &str) {
        let Some(host) = Url::parse(link).ok().and_then(|url| hostname(&url)) else {
            return;
        };
        self.add(&format!("url:{host}"));

        if host.parse::<IpAddr>().is_ok() {
            return;
        }
        let Some(registrable) = psl::domain_str(&host) else {
            return;
        };

        let mut rest = host.as_str();
        while rest != registrable {
            let Some((_, parent)) = rest.split_once('.') else {
                break;
            };
            rest = parent;
            self.add(&format!("url:{rest}"));
        }
    }
}

/// The host without the brackets of an IPv6 literal, lowercased.
fn hostname(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    (!host.is_empty()).then(|| host.to_lowercase())
}

/// Whether `c` is written without spaces between words: Han ideographs,
/// hiragana and Thai. Katakana words and Hangul are left to the word rules.
fn is_unspaced_script(c: char) -> bool {
    matches!(
        u32::from(c),
        0x0E00..=0x0E7F
            | 0x3040..=0x309F
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2FA1F
    )
}

/// The visible text of an HTML part, then its link targets. Script and style
/// contents are hidden.
fn strip_html(text: &str) -> String {
    let sink = TextSink::default();
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());

    let input = BufferQueue::default();
    input.push_back(StrTendril::from_slice(text));
    let _ = tokenizer.feed(&input);
    tokenizer.end();

    let sink = tokenizer.sink;
    let mut out = sink.visible.into_inner();
    out.push_str(&sink.links.into_inner());
    out
}

#[derive(Default)]
struct TextSink {
    visible: RefCell<String>,
    links: RefCell<String>,
    hidden: RefCell<i32>,
}

impl TokenSink for TextSink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        match token {
            CharacterTokens(text) => {
                if *self.hidden.borrow() == 0 {
                    self.visible.borrow_mut().push_str(&text);
                }
                TokenSinkResult::Continue
            }
            TagToken(tag) => {
                let hidden_tag = matches!(&*tag.name, "script" | "style");
                if hidden_tag && !tag.self_closing {
                    *self.hidden.borrow_mut() += if tag.kind == TagKind::StartTag { 1 } else { -1 };
                }
                for attr in &tag.attrs {
                    if &*attr.name.local == "href" {
                        let mut links = self.links.borrow_mut();
                        links.push(' ');
                        links.push_str(&attr.value);
                    }
                }
                self.visible.borrow_mut().push(' ');

                if hidden_tag && tag.kind == TagKind::StartTag && !tag.self_closing {
                    let kind = if &*tag.name == "script" {
                        RawKind::ScriptData
                    } else {
                        RawKind::Rawtext
                    };
                    return TokenSinkResult::RawData(kind);
                }
                TokenSinkResult::Continue
            }
            _ => TokenSinkResult::Continue,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a test that cannot build its message should panic"
)]
mod tests {
    use super::tokenize;

    fn tokens(subject: &str, body: &str) -> Vec<String> {
        let raw = format!(
            "From: a@example.com\r\nSubject: {subject}\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
        );
        tokenize(raw.as_bytes())
    }

    fn body(text: &str) -> Vec<String> {
        tokens("x", text)
    }

    fn has(tokens: &[String], wanted: &[&str]) -> bool {
        wanted.iter().all(|w| tokens.iter().any(|t| t == w))
    }

    #[test]
    fn contractions_and_decimals_stay_whole() {
        let t = body("don't pay 3.14 now");
        assert!(has(&t, &["don't", "pay", "3.14", "now"]), "{t:?}");
        assert!(!has(&t, &["don"]), "{t:?}");
    }

    #[test]
    fn chinese_becomes_overlapping_pairs() {
        let t = body("您的账户已被冻结");
        assert!(
            has(
                &t,
                &["您的", "的账", "账户", "户已", "已被", "被冻", "冻结"]
            ),
            "{t:?}"
        );
        assert!(!has(&t, &["您"]), "{t:?}");
    }

    #[test]
    fn punctuation_ends_a_run_of_pairs() {
        let t = body("冻结。验证");
        assert!(has(&t, &["冻结", "验证"]), "{t:?}");
        assert!(!has(&t, &["结验"]), "{t:?}");
    }

    #[test]
    fn a_lone_ideograph_is_kept() {
        let t = body("好 hello");
        assert!(has(&t, &["好", "hello"]), "{t:?}");
    }

    #[test]
    fn words_of_other_scripts_are_not_paired() {
        let t = body("Hello 世界 world");
        assert!(has(&t, &["hello", "世界", "world"]), "{t:?}");
        assert!(!has(&t, &["o世", "界w"]), "{t:?}");
    }

    #[test]
    fn japanese_pairs_kana_and_kanji_but_keeps_katakana_words() {
        let t = body("今すぐ確認 アカウント");
        assert!(
            has(&t, &["今す", "すぐ", "ぐ確", "確認", "アカウント"]),
            "{t:?}"
        );
    }

    #[test]
    fn thai_pairs_syllable_clusters() {
        let t = body("บัญชี");
        assert!(has(&t, &["บัญ", "ญชี"]), "{t:?}");
    }

    #[test]
    fn subject_pairs_are_prefixed() {
        let t = tokens("账户冻结", "hello");
        assert!(
            has(&t, &["subject:账户", "subject:户冻", "subject:冻结"]),
            "{t:?}"
        );
    }

    #[test]
    fn korean_words_are_unchanged() {
        let t = body("계정이 정지되었습니다");
        assert!(has(&t, &["계정이", "정지되었습니다"]), "{t:?}");
    }

    #[test]
    fn long_words_still_collapse_to_skip_tokens() {
        let t = body("abcdefghijklmnopqrstuvwxyz");
        assert!(has(&t, &["skip:a 20"]), "{t:?}");
    }
}
