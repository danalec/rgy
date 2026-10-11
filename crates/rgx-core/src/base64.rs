//! Base64 encode/decode, vendored from upstream
//! (`src/b02/encoding/base64.rs`, `b02::encoding`).
//!
//! In-house replacement for the `base64` crate. The upstream source already
//! provided the strict RFC 4648 §3.5 path added upstream
//! (`base64_decode_strict`, `base64_decode_bytes_strict`); rgx uses the
//! strict entry points throughout because a decode *failure* is a format
//! signal for the `tiktoken` adapter and a content guard for MIME
//! attachments, and a lenient decoder would silently widen both.
//!
//! The file is otherwise unmodified from upstream apart from:
//! - `alloc::` imports rewritten to `std::` (rgx is `std`-only);
//! - `core::error::Error` kept as-is (stable since 1.81).

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

#[derive(Debug, Clone)]
pub struct Base64Error {
    pub message: String,
    pub position: usize,
}

impl fmt::Display for Base64Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Base64 error at position {}: {}",
            self.position, self.message
        )
    }
}

impl core::error::Error for Base64Error {}

#[derive(Debug, Clone, Copy)]
pub struct Base64Config {
    pub charset: &'static [u8; 64],
    pub pad: bool,
}

pub const STANDARD: Base64Config = Base64Config {
    charset: b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/",
    pad: true,
};

pub const STANDARD_NO_PAD: Base64Config = Base64Config {
    charset: b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/",
    pad: false,
};

pub const URL_SAFE: Base64Config = Base64Config {
    charset: b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_",
    pad: true,
};

pub const URL_SAFE_NO_PAD: Base64Config = Base64Config {
    charset: b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_",
    pad: false,
};

/// Pre-computed decode table for the URL-safe Base64 alphabet
/// (`A-Za-z0-9-_`, no padding). 0xFF marks invalid characters.
/// Built at compile time from `URL_SAFE_NO_PAD.charset` so the
/// standard alphabet never pays a 256-byte init at runtime.
const DECODE_TABLE_URL_SAFE: [u8; 256] = build_decode_table(URL_SAFE_NO_PAD.charset);

/// Pre-computed decode table for the standard Base64 alphabet
/// (`A-Za-z0-9+/`, padded).
const DECODE_TABLE_STANDARD: [u8; 256] = build_decode_table(STANDARD.charset);

const fn build_decode_table(charset: &[u8; 64]) -> [u8; 256] {
    let mut table = [0xFFu8; 256];
    let mut i = 0;
    while i < 64 {
        table[charset[i] as usize] = i as u8;
        i += 1;
    }
    table
}

fn decode_table_for(charset: &[u8; 64]) -> Cow<'_, [u8; 256]> {
    if charset == STANDARD.charset.as_slice() {
        Cow::Borrowed(&DECODE_TABLE_STANDARD)
    } else if charset == URL_SAFE_NO_PAD.charset.as_slice() {
        Cow::Borrowed(&DECODE_TABLE_URL_SAFE)
    } else {
        // Custom charset: build per call. None of the in-tree callers
        // hit this branch; the API supports it for completeness.
        Cow::Owned(build_decode_table(charset))
    }
}

pub fn base64_encode(input: &[u8]) -> String {
    base64_encode_with(input, STANDARD)
}

pub fn base64_encode_url(input: &[u8]) -> String {
    base64_encode_with(input, URL_SAFE_NO_PAD)
}

pub fn base64_encode_with(input: &[u8], config: Base64Config) -> String {
    let charset = config.charset;
    let pad = config.pad;

    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);

    for chunk in input.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;

        output.push(charset[b0 >> 2] as char);
        output.push(charset[((b0 & 0x03) << 4) | (b1 >> 4)] as char);

        if chunk.len() > 1 {
            output.push(charset[((b1 & 0x0F) << 2) | (b2 >> 6)] as char);
            if chunk.len() > 2 {
                output.push(charset[b2 & 0x3F] as char);
            } else if pad {
                output.push('=');
            }
        } else if pad {
            output.push('=');
            output.push('=');
        }
    }

    output
}

pub fn base64_decode(input: &str) -> Result<Vec<u8>, Base64Error> {
    base64_decode_with(input, STANDARD)
}

pub fn base64_decode_url(input: &str) -> Result<Vec<u8>, Base64Error> {
    base64_decode_with(input, URL_SAFE_NO_PAD)
}

/// Strict RFC 4648 §3.5 decoding: canonical padding is required and
/// non-zero trailing bits in the final quantum are rejected.
///
/// The lenient `base64_decode` accepts unpadded input and silently drops
/// non-zero trailing bits, which is convenient for MIME but wrong for a
/// format check — `"Zh=="` would decode to `b"f"` even though the only
/// canonical encoding of `f` is `"Zg=="`.
pub fn base64_decode_strict(input: &str) -> Result<Vec<u8>, Base64Error> {
    base64_decode_with_strict(input, STANDARD)
}

pub fn base64_decode_with(input: &str, config: Base64Config) -> Result<Vec<u8>, Base64Error> {
    // `decode_bytes` already filters ASCII whitespace inline; a pre-clean
    // pass would just heap-allocate the same buffer twice. The early
    // bytes-as-`&[u8]` view avoids one allocation and a copy.
    decode_bytes(input.as_bytes(), config, false)
}

pub fn base64_decode_with_strict(
    input: &str,
    config: Base64Config,
) -> Result<Vec<u8>, Base64Error> {
    decode_bytes(input.as_bytes(), config, true)
}

/// Decode from raw bytes, so callers holding a byte buffer do not have to
/// round-trip through `&str` first. Whitespace is filtered exactly as in
/// the `&str` path.
pub fn base64_decode_bytes_strict(
    input: &[u8],
    config: Base64Config,
) -> Result<Vec<u8>, Base64Error> {
    decode_bytes(input, config, true)
}

/// Lenient decode of raw bytes for MIME bodies, where line breaks are
/// legal whitespace between base64 quanta. Errors are returned, but
/// the typical caller (mail / mbox adapters) only cares about whether
/// the decode succeeded.
pub fn base64_decode_skipping_ws(
    input: &[u8],
    config: Base64Config,
) -> Result<Vec<u8>, Base64Error> {
    decode_bytes(input, config, false)
}

fn decode_bytes(input: &[u8], config: Base64Config, strict: bool) -> Result<Vec<u8>, Base64Error> {
    let charset = config.charset;
    // The hot path is a slice with no whitespace -- a config blob or a
    // header value that arrived straight from the caller. The check is
    // one pass over the input; if it sees any byte that is whitespace,
    // fall back to the filtered `Vec` path that the comment below
    // describes. A base64 without line breaks (the common MIME body
    // case after header concatenation) pays nothing for this scan; a
    // stream with 76-char line breaks allocates once instead of taking
    // the always-on allocation.
    let owned_buf;
    let input_trimmed: &[u8] = if input.iter().any(|&b| b.is_ascii_whitespace()) {
        // Skip MIME whitespace (CR/LF/space/tab) inline rather than
        // building a filtered `Vec<u8>` first. The caller passes the raw
        // body bytes of a MIME part, which is mostly base64 with line
        // breaks every 76 chars; allocating a second copy just to drop
        // those breaks would double the peak heap on every part. The
        // strict path here still rejects non-whitespace invalid bytes;
        // only `is_ascii_whitespace` is dropped.
        let mut buf = Vec::with_capacity(input.len());
        for &b in input {
            if !b.is_ascii_whitespace() {
                buf.push(b);
            }
        }
        owned_buf = buf;
        &owned_buf
    } else {
        input
    };
    // The padding count is read off the input *with* the trailing
    // `=` still attached, since the strict path's expected vs actual
    // comparison is in terms of the padded form the caller sent. After
    // this we strip the trailing `=` so the data-path chunks iterate
    // over only the data characters.
    let padding_count = input_trimmed.iter().filter(|&&c| c == b'=').count();
    let input_trimmed = strip_trailing_padding(input_trimmed);

    if input_trimmed.len() % 4 == 1 {
        return Err(Base64Error {
            message: "Invalid length".into(),
            position: 0,
        });
    }

    if strict {
        // Canonical padding: a quantum of n data chars carries exactly
        // 4 - n '=' characters, and nothing else is allowed. This is the
        // check the lenient path deliberately skips.
        let rem = input_trimmed.len() % 4;
        let expected_padding = match rem {
            0 => 0,
            2 => 2,
            3 => 1,
            _ => 0,
        };
        if padding_count != expected_padding {
            return Err(Base64Error {
                message: format!(
                    "Invalid padding: expected {expected_padding}, found {padding_count}"
                ),
                position: input_trimmed.len(),
            });
        }
    } else if input_trimmed.len() % 4 == 1 && padding_count == 0 {
        return Err(Base64Error {
            message: "Invalid length".into(),
            position: 0,
        });
    }

    let decode_table = decode_table_for(charset);

    let mut output = Vec::with_capacity(input_trimmed.len() / 4 * 3);

    for (chunk_idx, chunk) in input_trimmed.chunks(4).enumerate() {
        let chunk_len = chunk.len();
        let is_last_chunk = chunk_idx + 1 >= input_trimmed.len().div_ceil(4);

        let v0 = decode_value(chunk[0], chunk_idx * 4, &decode_table)?;
        let v1 = decode_value(chunk[1], chunk_idx * 4 + 1, &decode_table)?;

        output.push((v0 << 2) | (v1 >> 4));

        if chunk_len >= 3 {
            let v2 = decode_value(chunk[2], chunk_idx * 4 + 2, &decode_table)?;
            output.push(((v1 & 0x0F) << 4) | (v2 >> 2));

            if chunk_len >= 4 {
                let v3 = decode_value(chunk[3], chunk_idx * 4 + 3, &decode_table)?;
                output.push(((v2 & 0x03) << 6) | v3);
            } else if strict && is_last_chunk && v2 & 0x03 != 0 {
                // trailing bits past the final byte must be zero
                return Err(Base64Error {
                    message: "Non-zero trailing bits in final quantum".into(),
                    position: input_trimmed.len(),
                });
            }
        } else if strict && is_last_chunk && v1 & 0x0F != 0 {
            return Err(Base64Error {
                message: "Non-zero trailing bits in final quantum".into(),
                position: input_trimmed.len(),
            });
        }
    }

    Ok(output)
}

fn strip_trailing_padding(input: &[u8]) -> &[u8] {
    let mut end = input.len();
    while end > 0 && input[end - 1] == b'=' {
        end -= 1;
    }
    &input[..end]
}

fn decode_value(c: u8, pos: usize, table: &[u8; 256]) -> Result<u8, Base64Error> {
    let v = table[c as usize];
    if v == 0xFF {
        return Err(Base64Error {
            message: format!("Invalid character '{}'", c as char),
            position: pos,
        });
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn strict_roundtrip() {
        let inputs: &[&[u8]] = &[
            b"",
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            b"foobar",
            &[0, 1, 2, 3, 255, 254, 253],
        ];
        for input in inputs {
            let encoded = base64_encode(input);
            assert_eq!(base64_decode_strict(&encoded).unwrap(), *input, "{encoded}");
        }
    }

    #[test]
    fn strict_rejects_missing_padding() {
        assert!(base64_decode("Zg").is_ok());
        assert!(base64_decode_strict("Zg").is_err());
        assert!(base64_decode_strict("Zm8").is_err());
        assert!(base64_decode_strict("Zm9v").is_ok(), "no padding needed");
    }

    #[test]
    fn strict_rejects_wrong_padding_count() {
        assert!(base64_decode_strict("Zg=").is_err());
        assert!(base64_decode_strict("Zg===").is_err());
        assert!(base64_decode_strict("Zm8==").is_err());
        assert!(base64_decode_strict("====").is_err());
    }

    #[test]
    fn strict_rejects_non_zero_trailing_bits() {
        assert_eq!(base64_decode("Zh==").unwrap(), b"f");
        assert!(base64_decode_strict("Zh==").is_err());
        assert!(base64_decode_strict("Zm9=").is_err());
    }

    #[test]
    fn strict_rejects_bad_alphabet() {
        assert!(base64_decode_strict("Zm9v!").is_err());
        assert!(base64_decode_strict("Zg==$").is_err());
    }

    #[test]
    fn strict_ignores_interior_whitespace_like_mime() {
        assert_eq!(base64_decode_strict("Zm9v\nYmFy").unwrap(), b"foobar");
        assert_eq!(base64_decode_strict(" Z m 9 v ").unwrap(), b"foo");
    }

    #[test]
    fn decode_bytes_matches_str_path() {
        let s = "Zm9vYmFy";
        let b = s.as_bytes();
        assert_eq!(
            base64_decode_bytes_strict(b, STANDARD).unwrap(),
            base64_decode_strict(s).unwrap()
        );
        assert!(base64_decode_bytes_strict(b"\xff\xfe", STANDARD).is_err());
    }

    #[test]
    fn url_safe_roundtrip() {
        let input = b"\xff\xfe\xfd\xfc";
        let encoded = base64_encode_url(input);
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
        assert_eq!(base64_decode_url(&encoded).unwrap(), input);
    }
}
