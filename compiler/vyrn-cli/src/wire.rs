//! The one encoding of a value that crosses the host boundary as a String: a
//! list of fields, each its byte length in decimal ASCII, `:`, then its bytes.
//! `vyrn serve`'s packed response and `std/fetch`'s request and reply use it;
//! `std/fetch`, `wasi_net.c` and `web/wasi-min.js` write the same bytes.

/// Appends `field` to `out`.
pub fn push(out: &mut Vec<u8>, field: &[u8]) {
    out.extend_from_slice(field.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(field);
}

/// Every field of `packed`, in order; `None` when it is not a list of fields.
pub fn fields(packed: &[u8]) -> Option<Vec<&[u8]>> {
    let mut out = Vec::new();
    let mut rest = packed;
    while !rest.is_empty() {
        let colon = rest.iter().position(|&b| b == b':')?;
        let digits = &rest[..colon];
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let n: usize = std::str::from_utf8(digits).ok()?.parse().ok()?;
        let tail = &rest[colon + 1..];
        out.push(tail.get(..n)?);
        rest = &tail[n..];
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_round_trip_and_refuse_a_bad_length() {
        let mut out = Vec::new();
        for f in [&b""[..], b"ok", b"a:b", "\u{e9}".as_bytes()] {
            push(&mut out, f);
        }
        assert_eq!(out, b"0:2:ok3:a:b2:\xc3\xa9");
        let got = fields(&out).expect("a list of fields");
        assert_eq!(got, [&b""[..], b"ok", b"a:b", "\u{e9}".as_bytes()]);
        assert_eq!(fields(b""), Some(Vec::new()));
        for bad in [&b"3:ab"[..], b"x:a", b":a", b"2"] {
            assert_eq!(fields(bad), None, "{bad:?}");
        }
    }
}
