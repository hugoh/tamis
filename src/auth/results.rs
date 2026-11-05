#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Method {
    Dmarc,
    Dkim,
    Other,
}

#[derive(Debug)]
pub(super) struct Result {
    pub method: Method,
    pub value: String,
    properties: Vec<(String, String)>,
}

impl Result {
    pub fn property(&self, name: &str) -> Option<&str> {
        self.properties
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug)]
pub(super) struct Parsed {
    pub authserv_id: String,
    pub results: Vec<Result>,
}

/// Parses a comment-free `Authentication-Results` value. Returns `None` for
/// anything that is not valid RFC 8601.
pub(super) fn parse(value: &str) -> Option<Parsed> {
    let mut segments = split_unquoted(value, ';').into_iter();

    let first = split_unquoted(segments.next()?.trim(), ' ');
    let authserv_id = unquote(first.first()?);
    if authserv_id.is_empty() {
        return None;
    }

    let mut results = Vec::new();
    for segment in segments {
        let segment = segment.trim();
        if segment.is_empty() || segment.eq_ignore_ascii_case("none") {
            continue;
        }
        results.push(parse_result(segment)?);
    }

    Some(Parsed {
        authserv_id,
        results,
    })
}

fn parse_result(segment: &str) -> Option<Result> {
    let mut words = split_unquoted(segment, ' ').into_iter();
    let (method, value) = words
        .next()?
        .split_once('=')
        .map(|(m, v)| (m.to_lowercase(), v.to_lowercase()))?;
    let method = match method.split('/').next().unwrap_or_default() {
        "dmarc" => Method::Dmarc,
        "dkim" => Method::Dkim,
        _ => Method::Other,
    };

    let properties = words
        .filter_map(|word| {
            let (key, value) = word.split_once('=')?;
            Some((key.to_lowercase(), unquote(value)))
        })
        .collect();

    Some(Result {
        method,
        value,
        properties,
    })
}

/// Splits on `separator` outside double quotes, dropping empty pieces when the
/// separator is a space.
fn split_unquoted(text: &str, separator: char) -> Vec<&str> {
    let (mut pieces, mut start, mut quoted) = (Vec::new(), 0, false);
    for (index, ch) in text.char_indices() {
        if ch == '"' {
            quoted = !quoted;
        } else if ch == separator && !quoted {
            pieces.push(&text[start..index]);
            start = index + ch.len_utf8();
        }
    }
    pieces.push(&text[start..]);

    if separator == ' ' {
        pieces.retain(|piece| !piece.is_empty());
    }
    pieces
}

fn unquote(word: &str) -> String {
    word.strip_prefix('"')
        .and_then(|w| w.strip_suffix('"'))
        .unwrap_or(word)
        .to_owned()
}
