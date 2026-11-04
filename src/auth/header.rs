/// The unfolded values of every header called `name`, topmost first. Reading
/// stops at the first blank line.
pub(super) fn values(raw: &[u8], name: &str) -> Vec<String> {
    let text = String::from_utf8_lossy(raw);
    let mut headers: Vec<String> = Vec::new();

    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some(last) = headers.last_mut() {
                last.push(' ');
                last.push_str(line.trim());
            }
        } else {
            headers.push(line.to_owned());
        }
    }

    headers
        .iter()
        .filter_map(|header| header.split_once(':'))
        .filter(|(key, _)| key.trim().eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim().to_owned())
        .collect()
}

/// Removes RFC 5322 comments, which nest and may hide ";" from a parser that
/// does not track the nesting.
pub(super) fn strip_comments(header: &str) -> String {
    let mut out = String::with_capacity(header.len());
    let (mut depth, mut quoted, mut escaped) = (0_u32, false, false);

    for ch in header.chars() {
        if escaped {
            escaped = false;
            if depth == 0 {
                out.push(ch);
            }
        } else if ch == '\\' {
            escaped = true;
            if depth == 0 {
                out.push(ch);
            }
        } else if quoted {
            quoted = ch != '"';
            out.push(ch);
        } else if ch == '(' {
            depth += 1;
        } else if ch == ')' && depth > 0 {
            depth -= 1;
            if depth == 0 {
                out.push(' ');
            }
        } else if depth == 0 {
            quoted = ch == '"';
            out.push(ch);
        }
    }

    out
}
