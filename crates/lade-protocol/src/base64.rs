//! Standard (RFC 4648) padded base64, used by the wire path encoding and shared with the service
//! for the stored path shape and the upstream proxy credentials.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let n = group.len();
        let b = [
            group[0],
            *group.get(1).unwrap_or(&0),
            *group.get(2).unwrap_or(&0),
        ];
        let v = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= n {
                out.push(char::from(ALPHABET[((v >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn value(c: u8) -> Option<u32> {
    let v = match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    };
    Some(u32::from(v))
}

pub fn decode(text: &str) -> Option<Vec<u8>> {
    let s = text.as_bytes();
    if !s.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let quads = s.len() / 4;
    for (index, quad) in s.chunks(4).enumerate() {
        let last = index + 1 == quads;
        let pad: u8 = match (quad[2], quad[3]) {
            (b'=', b'=') if last => 2,
            (_, b'=') if last => 1,
            _ => 0,
        };
        let mut v = 0u32;
        for &c in &quad[..4 - usize::from(pad)] {
            v = (v << 6) | value(c)?;
        }
        v <<= 6 * u32::from(pad);
        let [_, b0, b1, b2] = v.to_be_bytes();
        if (pad == 1 && b2 != 0) || (pad == 2 && b1 != 0) {
            return None;
        }
        out.extend_from_slice(&[b0, b1, b2][..3 - usize::from(pad)]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_vectors_round_trip() {
        for (raw, coded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(raw.as_bytes()), coded);
            assert_eq!(decode(coded).unwrap(), raw.as_bytes());
        }
    }

    #[test]
    fn every_byte_value_is_lossless() {
        let bytes: Vec<u8> = (0..=255).chain((0..=255).rev()).collect();
        for n in 0..bytes.len() {
            assert_eq!(decode(&encode(&bytes[..n])).unwrap(), &bytes[..n]);
        }
    }

    #[test]
    fn non_canonical_spellings_reject() {
        for bad in [
            "Zg", "Zg=", "Zh==", "Zm9=", "Z===", "=Zg=", "Zg==Zg==", "Zm9v\n", "Zm-v", "Zm_v",
            "Zg= =",
        ] {
            assert_eq!(decode(bad), None, "{bad:?}");
        }
    }
}
