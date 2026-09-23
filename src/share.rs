//! Encodes and decodes the editor's program into a URL fragment, so a link
//! alone carries a program with no server round trip -- the fragment never
//! reaches the server. The format is a contract (owner, 2026-09-22):
//! [`FRAGMENT_PREFIX`] plus base64url (no padding) of the raw deflate of the
//! UTF-8 source. Renaming the prefix, the encoding, or these public names
//! strands every link already shared and the blog embed that loads programs
//! the same way.
//!
//! The browser calls a share link needs -- `Location`, `History`,
//! `window.confirm`, `Navigator.share`, `Navigator.clipboard` -- live in
//! `main.rs`; every decision here is a plain function, host-testable with no
//! browser.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The URL fragment's prefix marking a share link. A fragment lacking it is
/// not a share link, however its payload would decode -- see
/// `program_from_hash`.
pub const FRAGMENT_PREFIX: &str = "#p=";

/// The largest a decoded program may inflate to. A link is untrusted input,
/// and deflate inflates roughly 1000:1; without this limit a 2 MB fragment
/// could inflate to 2 GB.
pub const MAX_SOURCE_BYTES: usize = 1 << 20;

/// Encodes `source` into a share link's payload, without [`FRAGMENT_PREFIX`].
pub fn encode(source: &str) -> String {
    let compressed = miniz_oxide::deflate::compress_to_vec(source.as_bytes(), 9);
    URL_SAFE_NO_PAD.encode(compressed)
}

/// Decodes a share link's payload -- the fragment's content after
/// [`FRAGMENT_PREFIX`] -- back into a program. `None` for anything a link,
/// as untrusted input, must not be trusted to produce: a payload that is
/// not base64url, not deflate, larger than [`MAX_SOURCE_BYTES`] once
/// inflated, not UTF-8, or empty.
pub fn decode(payload: &str) -> Option<String> {
    let compressed = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let bytes =
        miniz_oxide::inflate::decompress_to_vec_with_limit(&compressed, MAX_SOURCE_BYTES).ok()?;
    let source = String::from_utf8(bytes).ok()?;
    (!source.is_empty()).then_some(source)
}

/// `Location::hash()`'s value (with its leading `#`) to the program it
/// carries. `Some` only for a [`FRAGMENT_PREFIX`] fragment whose payload
/// decodes -- the prefix is checked here, not left to `decode` alone, so a
/// caller can tell a fragment to ignore (no prefix) from a share link that
/// doesn't read (prefix present, decode failed).
pub fn program_from_hash(hash: &str) -> Option<String> {
    decode(hash.strip_prefix(FRAGMENT_PREFIX)?)
}

/// Builds a full share link: `page` (a URL without a fragment) plus
/// [`FRAGMENT_PREFIX`] and `source`'s encoding.
pub fn share_url(page: &str, source: &str) -> String {
    format!("{page}{FRAGMENT_PREFIX}{}", encode(source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::examples::{DEFAULT_MMS, HELLO_WORLD_MMS};

    const SPECIAL_CHARACTERS_MMS: &str = "% café ✓\n\tLOC\t#100\n";
    const FIXTURE_PROGRAMS: [&str; 3] = [DEFAULT_MMS, HELLO_WORLD_MMS, SPECIAL_CHARACTERS_MMS];

    #[test]
    fn decode_of_encode_round_trips_every_fixture_program() {
        for program in FIXTURE_PROGRAMS {
            assert_eq!(
                decode(&encode(program)).as_deref(),
                Some(program),
                "round trip must recover {program:?}"
            );
        }
    }

    #[test]
    fn encode_uses_only_url_safe_characters() {
        for program in FIXTURE_PROGRAMS {
            let payload = encode(program);
            assert!(
                payload
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "payload must use only URL-safe characters: {payload:?}"
            );
        }
    }

    #[test]
    fn decode_rejects_a_payload_that_is_not_base64url() {
        assert_eq!(decode("!!!"), None);
    }

    #[test]
    fn decode_rejects_valid_base64url_that_is_not_deflate() {
        let payload = URL_SAFE_NO_PAD.encode(b"not a deflate stream");
        assert_eq!(decode(&payload), None);
    }

    #[test]
    fn decode_rejects_an_empty_payload() {
        assert_eq!(decode(""), None);
    }

    #[test]
    fn decode_rejects_a_payload_past_the_inflate_limit() {
        let huge = "\0".repeat(2 << 20);
        assert!(decode(&encode(&huge)).is_none());
    }

    #[test]
    fn decode_rejects_deflated_invalid_utf8() {
        let compressed = miniz_oxide::deflate::compress_to_vec(&[0xFF, 0xFE], 9);
        let payload = URL_SAFE_NO_PAD.encode(compressed);
        assert_eq!(decode(&payload), None);
    }

    #[test]
    fn program_from_hash_requires_the_p_prefix_and_a_decodable_payload() {
        let payload = encode(DEFAULT_MMS);
        for hash in [
            "",
            "#",
            "#x=abc",
            "#p=",
            &format!("#{payload}"),
            &format!("#x={payload}"),
        ] {
            assert_eq!(program_from_hash(hash), None, "must reject {hash:?}");
        }
    }

    #[test]
    fn program_from_hash_reads_a_well_formed_fragment() {
        for program in [DEFAULT_MMS, HELLO_WORLD_MMS] {
            let hash = format!("{FRAGMENT_PREFIX}{}", encode(program));
            assert_eq!(program_from_hash(&hash).as_deref(), Some(program));
        }
    }

    #[test]
    fn share_url_builds_a_fragment_program_from_hash_can_read_back() {
        let page = "https://playmmix.2ad.com/";
        let url = share_url(page, DEFAULT_MMS);
        assert!(
            url.starts_with(&format!("{page}{FRAGMENT_PREFIX}")),
            "must start with the page and {FRAGMENT_PREFIX:?}: {url}"
        );
        let fragment = &url[page.len()..];
        assert_eq!(program_from_hash(fragment).as_deref(), Some(DEFAULT_MMS));
    }
}
