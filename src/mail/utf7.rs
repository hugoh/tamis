//! IMAP's modified UTF-7 (RFC 3501 §5.1.3), which servers use for folder
//! names that aren't ASCII.

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

/// Folder names as sent: non-ASCII runs become `&<base64 of UTF-16BE>-`.
#[must_use]
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending: Vec<u16> = Vec::new();
    for c in name.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut out, &mut pending);
            out.push(c);
            if c == '&' {
                out.push('-');
            }
        } else {
            let mut units = [0; 2];
            pending.extend_from_slice(c.encode_utf16(&mut units));
        }
    }
    flush(&mut out, &mut pending);
    out
}

fn flush(out: &mut String, pending: &mut Vec<u16>) {
    if pending.is_empty() {
        return;
    }
    let bytes: Vec<u8> = pending.drain(..).flat_map(u16::to_be_bytes).collect();
    out.push('&');
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        let symbols = [18, 12, 6, 0];
        for shift in symbols.iter().take(chunk.len() + 1) {
            out.push(char::from(BASE64[((n >> shift) & 63) as usize]));
        }
    }
    out.push('-');
}

/// Folder names as shown. Anything that isn't valid modified UTF-7 is kept
/// as it came, since a server may send plain UTF-8.
#[must_use]
pub fn decode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('-') else {
            out.push_str(&rest[start..]);
            return out;
        };
        match &after[..end] {
            "" => out.push('&'),
            run => match decode_run(run) {
                Some(text) => out.push_str(&text),
                None => out.push_str(&rest[start..start + end + 2]),
            },
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn decode_run(run: &str) -> Option<String> {
    let mut bits = 0u32;
    let mut count = 0;
    let mut bytes = Vec::new();
    for c in run.bytes() {
        let value = u32::try_from(BASE64.iter().position(|b| *b == c)?).ok()?;
        bits = (bits << 6) | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push(u8::try_from((bits >> count) & 0xff).ok()?);
        }
    }
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks(2)
        .map(|p| u16::from_be_bytes([p[0], p[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_through_the_wire_form() {
        for (shown, wire) in [
            ("INBOX", "INBOX"),
            ("Réunions", "R&AOk-unions"),
            ("🛍\u{fe0f} shopping", "&2D3ezf4P- shopping"),
            ("Tom & Jerry", "Tom &- Jerry"),
            ("日本語", "&ZeVnLIqe-"),
            ("work/jobsearch", "work/jobsearch"),
        ] {
            assert_eq!(encode(shown), wire, "encode {shown}");
            assert_eq!(decode(wire), shown, "decode {wire}");
        }
    }

    #[test]
    fn what_is_not_modified_utf7_is_left_alone() {
        assert_eq!(decode("Café"), "Café");
        assert_eq!(decode("odd & ends"), "odd & ends");
        assert_eq!(decode("&!!-"), "&!!-");
    }
}
