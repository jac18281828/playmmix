//! Line tokenizer backing the editor's highlight overlay. Cosmetic only —
//! a lightweight per-line scan, not a reuse of checksmix's `pest` grammar
//! (`$CHECKSMIX/src/mmixal.pest` is read-only reference for rule names). MMIX
//! source has no multi-line constructs (no block comments, no multi-line
//! strings), so each line classifies independently of its neighbors.

use std::collections::HashSet;
use std::sync::LazyLock;

/// One highlighted category, each backed by its own CSS class so tests and
/// styling can target them independently (`playmmix-s3-editor-pane.md` §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Comment,
    String,
    Char,
    Register,
    Label,
    Keyword,
}

impl TokenKind {
    /// The CSS class the overlay applies to a span of this kind.
    pub fn css_class(self) -> &'static str {
        match self {
            TokenKind::Comment => "tok-comment",
            TokenKind::String => "tok-string",
            TokenKind::Char => "tok-char",
            TokenKind::Register => "tok-register",
            TokenKind::Label => "tok-label",
            TokenKind::Keyword => "tok-keyword",
        }
    }
}

/// A classified region of one line, as byte offsets into that line's `&str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub kind: TokenKind,
}

/// MMIXAL mnemonics and directives, case-insensitive. Extracted from the
/// quoted string literals in `mnemonic_*`/`directive_*` rules of
/// `$CHECKSMIX/src/mmixal.pest` with:
///
/// ```sh
/// grep -E '^(mnemonic|directive)_' src/mmixal.pest \
///   | grep -oE '\^"[^"]+"' \
///   | sed -E 's/^\^"//; s/"$//' \
///   | tr '[:upper:]' '[:lower:]' \
///   | sort -u
/// ```
///
/// This walks rule *names*, not line numbers, and reads every alternation a
/// rule carries (e.g. `directive_octa` yields `octa`, `.octa`, `quad`, and
/// `.quad`), so it survives grammar edits that only add or reorder rules.
/// Re-run it against a newer `mmixal.pest` if the keyword set drifts.
///
/// `debug` is added by hand: it is a source preprocessor directive
/// (`Self::preprocess_debug` in `$CHECKSMIX/src/mmixal.rs`), not a grammar
/// rule, so the extraction above never finds it.
static KEYWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        ".byte", ".octa", ".quad", ".tetra", ".wyde", "16addu", "16addui", "2addu", "2addui",
        "4addu", "4addui", "8addu", "8addui", "add", "addi", "addu", "addui", "and", "andi",
        "andn", "andnh", "andni", "andnl", "andnmh", "andnml", "bdif", "bdifi", "bev", "bevb",
        "bn", "bnb", "bnn", "bnnb", "bnp", "bnpb", "bnz", "bnzb", "bod", "bodb", "bp", "bpb",
        "byte", "bz", "bzb", "cmp", "cmpi", "cmpu", "cmpui", "csev", "csevi", "csn", "csni",
        "csnn", "csnni", "csnp", "csnpi", "csnz", "csnzi", "csod", "csodi", "csp", "cspi", "cswap",
        "cswapi", "csz", "cszi", "debug", "div", "divi", "divu", "divui", "fadd", "fcmp", "fcmpe",
        "fdiv", "feql", "feqle", "fint", "fix", "fixu", "flot", "floti", "flotu", "flotui", "fmul",
        "frem", "fsqrt", "fsub", "fun", "fune", "get", "geta", "getab", "go", "goi", "greg",
        "halt", "inch", "incl", "incmh", "incml", "is", "je", "jg", "jl", "jmp", "jne", "lda",
        "ldai", "ldb", "ldbi", "ldbu", "ldbui", "ldht", "ldhti", "ldo", "ldoi", "ldou", "ldoui",
        "ldsf", "ldsfi", "ldt", "ldti", "ldtu", "ldtui", "ldunc", "ldunci", "ldvts", "ldvtsi",
        "ldw", "ldwi", "ldwu", "ldwui", "loc", "mor", "mori", "mul", "muli", "mulu", "mului",
        "mux", "muxi", "mxor", "mxori", "nand", "nandi", "neg", "negi", "negu", "negui", "nor",
        "nori", "nxor", "nxori", "octa", "odif", "odifi", "or", "orh", "ori", "orl", "ormh",
        "orml", "orn", "orni", "pbev", "pbevb", "pbn", "pbnb", "pbnn", "pbnnb", "pbnp", "pbnpb",
        "pbnz", "pbnzb", "pbod", "pbodb", "pbp", "pbpb", "pbz", "pbzb", "pop", "prefix", "prego",
        "pregoi", "preld", "preldi", "prest", "presti", "pushgo", "pushgoi", "pushj", "pushjb",
        "put", "puti", "quad", "resume", "sadd", "saddi", "save", "set", "seth", "seti", "setl",
        "setmh", "setml", "sflot", "sfloti", "sflotu", "sflotui", "sl", "sli", "slu", "slui", "sr",
        "sri", "sru", "srui", "stb", "stbi", "stbu", "stbui", "stco", "stcoi", "stht", "sthti",
        "sto", "stoi", "stou", "stoui", "stsf", "stsfi", "stt", "stti", "sttu", "sttui", "stunc",
        "stunci", "stw", "stwi", "stwu", "stwui", "sub", "subi", "subu", "subui", "swym", "sync",
        "syncd", "syncdi", "syncid", "syncidi", "tdif", "tdifi", "tetra", "trap", "trip", "unsave",
        "wdif", "wdifi", "wyde", "xor", "xori", "zsev", "zsevi", "zsn", "zsni", "zsnn", "zsnni",
        "zsnp", "zsnpi", "zsnz", "zsnzi", "zsod", "zsodi", "zsp", "zspi", "zsz", "zszi",
    ]
    .into_iter()
    .collect()
});

fn is_keyword(word: &str) -> bool {
    KEYWORDS.contains(word.to_ascii_lowercase().as_str())
}

/// Classify one line of MMIX source into highlighted spans. Byte offsets are
/// relative to `line` and always fall on `char` boundaries.
pub fn classify(line: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut chars = line.char_indices().peekable();
    let mut first_token_seen = false;

    while let Some((i, c)) = chars.next() {
        match c {
            // COMMENT: ';' or '%' to end of line. Reached only outside a
            // string/char literal, since those are consumed whole by the
            // '"' and '\'' arms below before this arm ever sees their
            // contents.
            ';' | '%' => {
                spans.push(Span {
                    start: i,
                    end: line.len(),
                    kind: TokenKind::Comment,
                });
                break;
            }
            // string_literal: "...", no escape sequences.
            '"' => {
                let mut end = line.len();
                for (j, cj) in chars.by_ref() {
                    if cj == '"' {
                        end = j + cj.len_utf8();
                        break;
                    }
                }
                spans.push(Span {
                    start: i,
                    end,
                    kind: TokenKind::String,
                });
            }
            // char_literal: 'x' or '\n' | '\r' | '\t' | '\0' | '\\' | '\''.
            '\'' => {
                let mut end = line.len();
                if let Some(&(_, next)) = chars.peek() {
                    chars.next();
                    if next == '\\' {
                        chars.next(); // the escaped character, if present.
                    }
                }
                if let Some(&(j, cj)) = chars.peek()
                    && cj == '\''
                {
                    chars.next();
                    end = j + cj.len_utf8();
                }
                spans.push(Span {
                    start: i,
                    end,
                    kind: TokenKind::Char,
                });
            }
            // register_num: '$' followed by one or more digits.
            '$' => {
                let mut end = i + c.len_utf8();
                let mut has_digit = false;
                while let Some(&(j, cj)) = chars.peek() {
                    if !cj.is_ascii_digit() {
                        break;
                    }
                    has_digit = true;
                    end = j + cj.len_utf8();
                    chars.next();
                }
                if has_digit {
                    spans.push(Span {
                        start: i,
                        end,
                        kind: TokenKind::Register,
                    });
                }
            }
            // Identifier-shaped token: a directive keyword may carry a
            // leading '.' (".BYTE"); labels and mnemonics never do.
            c if c == '.' || c.is_ascii_alphabetic() || c == '_' => {
                let mut end = i + c.len_utf8();
                while let Some(&(j, cj)) = chars.peek() {
                    if !(cj.is_ascii_alphanumeric() || cj == '_') {
                        break;
                    }
                    end = j + cj.len_utf8();
                    chars.next();
                }
                let word = &line[i..end];
                let keyword = is_keyword(word);
                // The grammar's `statement` rule tries directive, then
                // instruction, then label_def: the first token on a line is
                // a label unless it names a keyword. Every later keyword
                // match (the mnemonic after a label, or a directive name)
                // still gets `.tok-keyword`; later non-keyword identifiers
                // (operands, symbolic register names) are left unstyled.
                if !first_token_seen {
                    first_token_seen = true;
                    spans.push(Span {
                        start: i,
                        end,
                        kind: if keyword {
                            TokenKind::Keyword
                        } else {
                            TokenKind::Label
                        },
                    });
                } else if keyword {
                    spans.push(Span {
                        start: i,
                        end,
                        kind: TokenKind::Keyword,
                    });
                }
            }
            _ => {}
        }
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span_text<'a>(line: &'a str, span: &Span) -> &'a str {
        &line[span.start..span.end]
    }

    #[test]
    fn classifies_label_and_string_from_hello_world() {
        let line = r#"Text	BYTE	"Hello world!",'\n',0"#;
        let spans = classify(line);

        let label = spans
            .iter()
            .find(|s| s.kind == TokenKind::Label)
            .expect("label span");
        assert_eq!(span_text(line, label), "Text");

        let string = spans
            .iter()
            .find(|s| s.kind == TokenKind::String)
            .expect("string span");
        assert_eq!(span_text(line, string), r#""Hello world!""#);
    }

    #[test]
    fn comment_delimiter_inside_a_string_is_not_a_comment() {
        let line = "X\tBYTE\t\"100%\"\t% real comment";
        let spans = classify(line);

        let string = spans
            .iter()
            .find(|s| s.kind == TokenKind::String)
            .expect("string span");
        assert_eq!(span_text(line, string), "\"100%\"");

        let comment = spans
            .iter()
            .find(|s| s.kind == TokenKind::Comment)
            .expect("comment span");
        assert_eq!(span_text(line, comment), "% real comment");
    }

    #[test]
    fn mnemonic_after_label_is_a_keyword() {
        let line = "Main\tdebug \"hi\"";
        let spans = classify(line);

        assert_eq!(
            spans
                .iter()
                .find(|s| s.kind == TokenKind::Label)
                .map(|s| span_text(line, s)),
            Some("Main")
        );
        assert_eq!(
            spans
                .iter()
                .find(|s| s.kind == TokenKind::Keyword)
                .map(|s| span_text(line, s)),
            Some("debug")
        );
    }

    #[test]
    fn bare_mnemonic_line_has_no_label() {
        let line = "\tLDA\t\t$255,Text";
        let spans = classify(line);

        assert!(!spans.iter().any(|s| s.kind == TokenKind::Label));
        assert_eq!(
            spans
                .iter()
                .find(|s| s.kind == TokenKind::Keyword)
                .map(|s| span_text(line, s)),
            Some("LDA")
        );
        let register = spans
            .iter()
            .find(|s| s.kind == TokenKind::Register)
            .expect("register span");
        assert_eq!(span_text(line, register), "$255");
    }

    #[test]
    fn char_literal_with_escape() {
        let line = r"'\n'";
        let spans = classify(line);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, TokenKind::Char);
        assert_eq!(span_text(line, &spans[0]), r"'\n'");
    }

    #[test]
    fn dotted_directive_is_a_keyword() {
        let line = "\t.BYTE\t1,2,3";
        let spans = classify(line);
        assert_eq!(
            spans
                .iter()
                .find(|s| s.kind == TokenKind::Keyword)
                .map(|s| span_text(line, s)),
            Some(".BYTE")
        );
    }
}
