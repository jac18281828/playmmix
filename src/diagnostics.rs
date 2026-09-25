//! Source-error diagnostics: a MIX-vs-MMIX heuristic and error-location
//! parsing.

use crate::SOURCE_FILENAME;
use crate::highlight::{self, TokenKind};

/// MIX-only opcodes: mnemonics classic MIX has but MMIXAL doesn't, so their
/// presence as a whole token is a strong signal the pasted source is MIX,
/// not MMIXAL. The register-sign/zero/overflow jump family (`JAN`, `JAZ`,
/// `JAP`, `JANN`, `JANZ`, `JANP`, `JAO`) repeats for registers 1-6 and X;
/// `JNOV` has no per-register form.
const MIX_ONLY_OPCODES: &[&str] = &[
    "ENTA", "ENTX", "ENNA", "ENNX", "CMPA", "CMP1", "CMP2", "CMP3", "CMP4", "CMP5", "CMP6", "INCA",
    "INCX", "DECA", "DECX", "SLAX", "SRAX", "SLC", "SRC", "HLT", "IOC", "JAN", "JAZ", "JAP",
    "JANN", "JANZ", "JANP", "JAO", "J1N", "J1Z", "J1P", "J1NN", "J1NZ", "J1NP", "J1O", "J2N",
    "J2Z", "J2P", "J2NN", "J2NZ", "J2NP", "J2O", "J3N", "J3Z", "J3P", "J3NN", "J3NZ", "J3NP",
    "J3O", "J4N", "J4Z", "J4P", "J4NN", "J4NZ", "J4NP", "J4O", "J5N", "J5Z", "J5P", "J5NN", "J5NZ",
    "J5NP", "J5O", "J6N", "J6Z", "J6P", "J6NN", "J6NZ", "J6NP", "J6O", "JXN", "JXZ", "JXP", "JXNN",
    "JXNZ", "JXNP", "JXO", "JNOV",
];

/// Whether `token` contains a MIX field-spec operand, `(<digits>:<digits>)`
/// -- MMIXAL never uses a parenthesized range. Scoped to one token (not a
/// raw substring search over the whole line) so a match can't span into an
/// unrelated token, e.g. a comment or string literal.
fn token_has_mix_field_spec(token: &str) -> bool {
    let mut rest = token;
    while let Some(open) = rest.find('(') {
        let after_open = &rest[open + 1..];
        match after_open.find(')') {
            Some(close) => {
                let inner = &after_open[..close];
                if let Some((left, right)) = inner.split_once(':') {
                    let is_digits =
                        |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
                    if is_digits(left) && is_digits(right) {
                        return true;
                    }
                }
                rest = &after_open[close + 1..];
            }
            None => return false,
        }
    }
    false
}

/// Blanks every byte of `line` that falls inside a `highlight::classify` span
/// of kind `Comment`, `String` or `Char`, leaving its length and every other
/// byte untouched. `classify`'s spans fall on `char` boundaries, and every
/// replacement byte is an ASCII space, so the result is always valid UTF-8.
/// Blanking rather than removing keeps the tokens on either side of an
/// excluded span from joining into one.
fn line_with_excluded_spans_blanked(line: &str) -> String {
    let mut bytes = line.as_bytes().to_vec();
    for span in highlight::classify(line) {
        if matches!(
            span.kind,
            TokenKind::Comment | TokenKind::String | TokenKind::Char
        ) {
            bytes[span.start..span.end].fill(b' ');
        }
    }
    String::from_utf8(bytes).expect("blanking with ASCII spaces preserves valid UTF-8")
}

/// A heuristic classifier for classic MIX source (Knuth's original
/// architecture, distinct from MMIX): checked only after MMIXAL parsing has
/// already failed, so it never changes what parses -- it only improves the
/// message when parsing was always going to fail. Matches whitespace-
/// delimited tokens outside any comment, string or character-constant span
/// (`line_with_excluded_spans_blanked`), so a MIX-only word inside one of
/// those can't false-positive.
fn looks_like_mix(source: &str) -> bool {
    source.lines().any(|line| {
        let line = line_with_excluded_spans_blanked(line);
        line.split_whitespace().any(|token| {
            token == "ORIG" || MIX_ONLY_OPCODES.contains(&token) || token_has_mix_field_spec(token)
        })
    })
}

/// The complete message to display for a load/reload error from
/// user-supplied source: the bare MIX sentence if `source` looks like MIX
/// (see `looks_like_mix`), or the normal "Assembly error: ..." otherwise --
/// decided once, here, so `view()` can render `self.error` verbatim with no
/// formatting of its own. Either way, every literal `"{SOURCE_FILENAME}:"`
/// is stripped from `error` first (see `strip_source_filename`) -- the
/// phantom filename is checksmix's own error text, not something the user
/// ever named.
pub(crate) fn describe_source_error(source: &str, error: &str) -> String {
    if looks_like_mix(source) {
        "This looks like MIX, not MMIX.".to_string()
    } else {
        format!("Assembly error: {}", strip_source_filename(error))
    }
}

/// Every literal occurrence of `"{SOURCE_FILENAME}:"` removed from `error` --
/// not just a leading prefix, since the symbol-redefinition shape embeds a
/// *second* `{SOURCE_FILENAME}:{line}` reference inside the message body
/// (`"... redefined (first defined at source.mms:2)"`), which a
/// prefix-only strip would leave untouched.
fn strip_source_filename(error: &str) -> String {
    error.replace(&format!("{SOURCE_FILENAME}:"), "")
}

/// Parses checksmix's raw error text for a `(line, column)` source
/// location, defensively -- checksmix's error shapes are not uniform:
///
/// - The common `pest`-parser syntax error:
///   `"{SOURCE_FILENAME}:{line}:{col}: {message}"` -- both a line and a
///   column.
/// - A symbol-redefinition error: `"{SOURCE_FILENAME}:{line}: symbol
///   '{name}' redefined (first defined at {SOURCE_FILENAME}:{line})"` --
///   a line only, no column, and a second, embedded
///   `{SOURCE_FILENAME}:{line}` reference later in the message that must
///   not be mistaken for this one.
/// - Everything else (e.g. `"Invalid opcode: {value}"`) -- no location at
///   all.
///
/// Tries the line-and-column prefix first, then the line-only prefix,
/// returning `None` if neither matches -- there is genuinely nothing to
/// point at. Only ever looks at a leading `"{SOURCE_FILENAME}:"` prefix, so
/// the redefinition shape's second, embedded reference is never mistaken
/// for the primary location.
pub(crate) fn parse_error_location(error: &str) -> Option<(usize, Option<usize>)> {
    let rest = error.strip_prefix(&format!("{SOURCE_FILENAME}:"))?;
    let (line_str, after_line) = rest.split_once(':')?;

    if let Some((col_str, after_col)) = after_line.split_once(':')
        && after_col.starts_with(' ')
        && let (Ok(line), Ok(col)) = (line_str.parse(), col_str.parse())
    {
        return Some((line, Some(col)));
    }
    if after_line.starts_with(' ')
        && let Ok(line) = line_str.parse()
    {
        return Some((line, None));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::control::Control;
    use crate::examples::DEFAULT_MMS;

    #[test]
    fn looks_like_mix_detects_orig_on_its_own() {
        // No MIX-only opcode here -- isolates the ORIG signal so a mutant
        // that deletes it (leaving only the opcode-table check) goes red,
        // instead of surviving behind the opcode signal in a combined test.
        let mix_source = "\tORIG\t3000\nSTART\tLDA\t0\n";
        assert!(looks_like_mix(mix_source));
    }

    #[test]
    fn looks_like_mix_detects_a_mix_only_opcode_on_its_own() {
        // No ORIG here -- isolates the opcode-table signal so a mutant that
        // deletes MIX_ONLY_OPCODES entirely goes red on its own.
        let mix_source = "START\tENTA\t0\n\tCMPA\t2000,1\n";
        assert!(looks_like_mix(mix_source));
    }

    #[test]
    fn looks_like_mix_detects_a_field_spec_operand() {
        let mix_source = "\tLDA\t6,X(1:3)\n";
        assert!(looks_like_mix(mix_source));
    }

    #[test]
    fn looks_like_mix_is_false_for_ordinary_mmixal() {
        assert!(!looks_like_mix(DEFAULT_MMS));
    }

    #[test]
    fn parse_error_location_recovers_line_and_column_from_the_common_shape() {
        let error = "source.mms:3:13: syntax error: expected one of: ...";
        assert_eq!(parse_error_location(error), Some((3, Some(13))));
    }

    #[test]
    fn parse_error_location_recovers_line_only_from_the_redefinition_shape() {
        // No column in this shape, and a *second* `filename:line` reference
        // embedded later in the message -- must not be mistaken for the
        // primary location.
        let error = "source.mms:5: symbol 'Foo' redefined (first defined at source.mms:2)";
        assert_eq!(parse_error_location(error), Some((5, None)));
    }

    #[test]
    fn parse_error_location_is_none_for_a_location_less_message() {
        let error = "Invalid opcode: 0x1a";
        assert_eq!(parse_error_location(error), None);
    }

    #[test]
    fn parse_error_location_recovers_a_location_for_real_mix_looking_source() {
        // checksmix's *raw* error for MIX-shaped input, not the friendly
        // "This looks like MIX" display string the parser never sees --
        // `looks_like_mix` only changes what text the user reads, not what
        // checksmix actually returned, so a location genuinely exists here
        // too.
        let mix_source = "\tORIG\t3000\nSTART\tLDA\t0\n";
        assert!(looks_like_mix(mix_source), "fixture must look like MIX");
        let error = match Control::new(mix_source, SOURCE_FILENAME) {
            Ok(_) => panic!("MIX-shaped source must fail MMIXAL assembly"),
            Err(error) => error,
        };
        assert!(
            parse_error_location(&error).is_some(),
            "a real checksmix parse failure must still yield a location: {error:?}"
        );
    }

    #[test]
    fn strip_source_filename_removes_every_occurrence() {
        let error = "source.mms:5: symbol 'Foo' redefined (first defined at source.mms:2)";
        let stripped = strip_source_filename(error);
        assert!(!stripped.contains(SOURCE_FILENAME));
    }

    #[test]
    fn describe_source_error_strips_the_phantom_filename() {
        let error = "source.mms:3:13: syntax error: ...";
        let message = describe_source_error("\tADDD\t$1,$2,$3\n", error);
        assert!(!message.contains(SOURCE_FILENAME), "{message}");
    }

    #[test]
    fn looks_like_mix_ignores_a_whole_word_orig_inside_a_percent_comment() {
        let source = "% Converted from ORIG 3000\n";
        assert!(!looks_like_mix(source));
    }

    #[test]
    fn looks_like_mix_ignores_a_mix_only_opcode_in_a_trailing_remark() {
        let source = "\tSWYM\t% comment CMPA\n";
        assert!(!looks_like_mix(source));
    }

    #[test]
    fn looks_like_mix_ignores_orig_inside_a_byte_string() {
        // Internal spaces around ORIG so it stands as its own
        // whitespace-delimited token inside the string -- unlike a bare
        // `"ORIG"`, whose attached quotes would already fail the token's
        // exact-match check even scanning the raw, unexcluded line.
        let source = "\tBYTE\t\"note ORIG note\"\n";
        assert!(!looks_like_mix(source));
    }

    #[test]
    fn looks_like_mix_never_substring_matches_across_tokens() {
        // "ORIG" and "CMPA" as substrings of unrelated, larger tokens (not
        // whitespace-delimited on their own) must not false-positive.
        let source = "; a comment mentioning PREORIGAMI and DECMPACT\n";
        assert!(!looks_like_mix(source));
    }
}
