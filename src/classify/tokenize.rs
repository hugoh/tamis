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
use mail_parser::{Addr, Address, MessageParser, MimeHeaders, PartType};
use regex::Regex;
use unicode_general_category::{GeneralCategory, get_general_category};
use url::Url;

use super::recipients::{MAX_RECIPIENTS, RECIPIENT_PREFIX};

const MIN_WORD_LEN: usize = 3;
const MAX_WORD_LEN: usize = 12;
const MAX_PART_BYTES: usize = 64 << 10;
const SKIP_BUCKET: usize = 10;

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

    for part in &message.parts {
        let (media, subtype) = part.content_type().map_or(("text", "plain"), |ct| {
            (ct.ctype(), ct.subtype().unwrap_or(""))
        });
        let is_html = media.eq_ignore_ascii_case("text") && subtype.eq_ignore_ascii_case("html");
        let is_plain = media.eq_ignore_ascii_case("text") && subtype.eq_ignore_ascii_case("plain");

        let body = match &part.body {
            PartType::Text(text) if is_plain => text,
            PartType::Html(text) if is_html => text,
            _ => continue,
        };

        let body = truncate(body, MAX_PART_BYTES);
        if is_html {
            tokens.add_text("", &strip_html(body));
        } else {
            tokens.add_text("", body);
        }
    }

    tokens.list
}

fn addresses(address: Option<&Address<'_>>) -> Vec<String> {
    address
        .into_iter()
        .flat_map(Address::iter)
        .filter_map(Addr::address)
        .map(str::to_lowercase)
        .collect()
}

/// Records who the message went to: each distinct To and Cc address and its
/// domain, up to `MAX_RECIPIENTS` addresses.
fn add_recipient_tokens(tokens: &mut TokenSet, to: Option<&Address<'_>>, cc: Option<&Address<'_>>) {
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

        for word in rest
            .to_lowercase()
            .split(|c: char| !is_letter_or_digit(c))
            .filter(|w| !w.is_empty())
        {
            let length = word.chars().count();
            if length < MIN_WORD_LEN {
                continue;
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

fn is_letter_or_digit(c: char) -> bool {
    use GeneralCategory::{
        DecimalNumber, LowercaseLetter, ModifierLetter, OtherLetter, TitlecaseLetter,
        UppercaseLetter,
    };
    matches!(
        get_general_category(c),
        UppercaseLetter
            | LowercaseLetter
            | TitlecaseLetter
            | ModifierLetter
            | OtherLetter
            | DecimalNumber
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
