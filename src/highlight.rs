//! Line tokenizer backing the editor's highlight overlay. Cosmetic only —
//! a lightweight per-line scan, not a reuse of checksmix's `pest` grammar
//! (`$CHECKSMIX/src/mmixal.pest` is read-only reference for rule names). MMIX
//! source has no multi-line constructs (no block comments, no multi-line
//! strings), so each line classifies independently of its neighbors.
//!
//! `classify`'s line model, checksmix 0.3.13: a line whose first character
//! isn't a blank, an ASCII letter, digit, `:` or `_` is a comment in full.
//! Otherwise the line is one or more `;`-separated statements, each LABEL,
//! OP, EXPR, then a remark. LABEL exists only where the statement starts in
//! column 1 or directly after `;`, and only when the first word there isn't
//! a keyword; an indented statement has no LABEL, so its first word is OP.
//! A statement whose first non-blank character isn't a letter, digit, `:`
//! or `_` is itself a remark. EXPR runs from OP to the first blank outside
//! a string, character constant or parenthesized group -- except a blank
//! run touching a comma, which stays inside -- styling only the strings,
//! character constants and registers it holds. The remaining text, up to
//! `;` or the end of the line, is a remark; one opening with `%` swallows
//! any `;` and runs to the end of the line instead.

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
    /// A non-keyword token in the mnemonic slot -- immediately after a
    /// recognized label, with nothing else between -- so a misspelled
    /// mnemonic reads as distinctly wrong rather than losing its
    /// syntax-highlight color like a legitimate unstyled operand. See
    /// `classify`'s `op_word` tracking.
    UnknownMnemonic,
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
            TokenKind::UnknownMnemonic => "tok-unknown-mnemonic",
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

/// MMIXAL mnemonics and directives, case-sensitive. Extracted from the
/// quoted string literals in `mnemonic_*`/`directive_*` rules of
/// `$CHECKSMIX/src/mmixal.pest` with:
///
/// ```sh
/// grep -E '^(mnemonic|directive)_' src/mmixal.pest \
///   | grep -oE '"[^"]+"' \
///   | sed -E 's/^"//; s/"$//' \
///   | sort -u
/// ```
///
/// This walks rule *names*, not line numbers, and reads every alternation a
/// rule carries, so it survives grammar edits that only add or reorder
/// rules. Re-run it against a newer `mmixal.pest` if the keyword set drifts.
///
/// `debug` is added by hand, lower-case: it is a source preprocessor
/// directive (`Self::preprocess_debug` in `$CHECKSMIX/src/mmixal.rs`), not a
/// grammar rule, so the extraction above never finds it. Every other keyword
/// matches only its upper-case spelling: the grammar has no case-insensitive
/// literals left.
static KEYWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "16ADDU", "16ADDUI", "2ADDU", "2ADDUI", "4ADDU", "4ADDUI", "8ADDU", "8ADDUI", "ADD",
        "ADDI", "ADDU", "ADDUI", "AND", "ANDI", "ANDN", "ANDNH", "ANDNI", "ANDNL", "ANDNMH",
        "ANDNML", "BDIF", "BDIFI", "BEV", "BEVB", "BN", "BNB", "BNN", "BNNB", "BNP", "BNPB", "BNZ",
        "BNZB", "BOD", "BODB", "BP", "BPB", "BSPEC", "BYTE", "BZ", "BZB", "CMP", "CMPI", "CMPU",
        "CMPUI", "CSEV", "CSEVI", "CSN", "CSNI", "CSNN", "CSNNI", "CSNP", "CSNPI", "CSNZ", "CSNZI",
        "CSOD", "CSODI", "CSP", "CSPI", "CSWAP", "CSWAPI", "CSZ", "CSZI", "DIV", "DIVI", "DIVU",
        "DIVUI", "ESPEC", "FADD", "FCMP", "FCMPE", "FDIV", "FEQL", "FEQLE", "FINT", "FIX", "FIXU",
        "FLOT", "FLOTI", "FLOTU", "FLOTUI", "FMUL", "FREM", "FSQRT", "FSUB", "FUN", "FUNE", "GET",
        "GETA", "GETAB", "GO", "GOI", "GREG", "HALT", "INCH", "INCL", "INCMH", "INCML", "IS",
        "JMP", "JMPB", "LDA", "LDAI", "LDB", "LDBI", "LDBU", "LDBUI", "LDHT", "LDHTI", "LDO",
        "LDOI", "LDOU", "LDOUI", "LDSF", "LDSFI", "LDT", "LDTI", "LDTU", "LDTUI", "LDUNC",
        "LDUNCI", "LDVTS", "LDVTSI", "LDW", "LDWI", "LDWU", "LDWUI", "LOC", "LOCAL", "MOR", "MORI",
        "MUL", "MULI", "MULU", "MULUI", "MUX", "MUXI", "MXOR", "MXORI", "NAND", "NANDI", "NEG",
        "NEGI", "NEGU", "NEGUI", "NOR", "NORI", "NXOR", "NXORI", "OCTA", "ODIF", "ODIFI", "OR",
        "ORH", "ORI", "ORL", "ORMH", "ORML", "ORN", "ORNI", "PBEV", "PBEVB", "PBN", "PBNB", "PBNN",
        "PBNNB", "PBNP", "PBNPB", "PBNZ", "PBNZB", "PBOD", "PBODB", "PBP", "PBPB", "PBZ", "PBZB",
        "POP", "PREFIX", "PREGO", "PREGOI", "PRELD", "PRELDI", "PREST", "PRESTI", "PUSHGO",
        "PUSHGOI", "PUSHJ", "PUSHJB", "PUT", "PUTI", "RESUME", "SADD", "SADDI", "SAVE", "SET",
        "SETH", "SETI", "SETL", "SETMH", "SETML", "SFLOT", "SFLOTI", "SFLOTU", "SFLOTUI", "SL",
        "SLI", "SLU", "SLUI", "SR", "SRI", "SRU", "SRUI", "STB", "STBI", "STBU", "STBUI", "STCO",
        "STCOI", "STHT", "STHTI", "STO", "STOI", "STOU", "STOUI", "STSF", "STSFI", "STT", "STTI",
        "STTU", "STTUI", "STUNC", "STUNCI", "STW", "STWI", "STWU", "STWUI", "SUB", "SUBI", "SUBU",
        "SUBUI", "SWYM", "SYNC", "SYNCD", "SYNCDI", "SYNCID", "SYNCIDI", "TDIF", "TDIFI", "TETRA",
        "TRAP", "TRIP", "UNSAVE", "WDIF", "WDIFI", "WYDE", "XOR", "XORI", "ZSEV", "ZSEVI", "ZSN",
        "ZSNI", "ZSNN", "ZSNNI", "ZSNP", "ZSNPI", "ZSNZ", "ZSNZI", "ZSOD", "ZSODI", "ZSP", "ZSPI",
        "ZSZ", "ZSZI",
        // The preprocessor's own directive, not a grammar rule (see above).
        "debug",
    ]
    .into_iter()
    .collect()
});

fn is_keyword(word: &str) -> bool {
    KEYWORDS.contains(word)
}

/// A symbol character: a label, mnemonic or directive name may hold ASCII
/// letters, digits, `_` and `:` anywhere (`:Main`, `Foo:Bar`); a `.` never
/// starts or continues one.
fn is_symbol_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == ':'
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// The byte offset `cs[idx]` starts at, or `line_len` past the last
/// character -- so a scan that walked off the end of `cs` still yields a
/// valid span end.
fn byte_at(cs: &[(usize, char)], idx: usize, line_len: usize) -> usize {
    cs.get(idx).map(|&(b, _)| b).unwrap_or(line_len)
}

/// Classify one line of MMIX source into highlighted spans. Byte offsets are
/// relative to `line` and always fall on `char` boundaries, disjoint and in
/// order.
pub fn classify(line: &str) -> Vec<Span> {
    let cs: Vec<(usize, char)> = line.char_indices().collect();
    let n = cs.len();
    let line_len = line.len();
    if n == 0 {
        return Vec::new();
    }

    // Whole-line comment: the line's own first character decides this once,
    // never re-checked per statement.
    let c0 = cs[0].1;
    if !(is_blank(c0) || is_symbol_char(c0)) {
        return vec![Span {
            start: 0,
            end: line_len,
            kind: TokenKind::Comment,
        }];
    }

    let mut spans = Vec::new();
    let mut i = 0usize;
    let mut first_statement = true;

    while i < n {
        // LABEL exists only where this statement starts in column 1 of the
        // line (the first statement, undented) or directly after `;`
        // (every later statement, with or without blanks).
        let label_eligible = if first_statement {
            !is_blank(cs[0].1)
        } else {
            true
        };
        first_statement = false;

        while i < n && is_blank(cs[i].1) {
            i += 1;
        }
        if i >= n {
            break;
        }
        if cs[i].1 == ';' {
            i += 1;
            continue;
        }

        // A statement whose first non-blank character isn't a symbol start
        // is itself a remark, up to `;` (or, opening with `%`, to the end
        // of the line, swallowing any `;`).
        let first_ch = cs[i].1;
        if !is_symbol_char(first_ch) {
            let remark_start = cs[i].0;
            if first_ch == '%' {
                spans.push(Span {
                    start: remark_start,
                    end: line_len,
                    kind: TokenKind::Comment,
                });
                break;
            }
            let mut j = i;
            while j < n && cs[j].1 != ';' {
                j += 1;
            }
            spans.push(Span {
                start: remark_start,
                end: byte_at(&cs, j, line_len),
                kind: TokenKind::Comment,
            });
            if j < n {
                i = j + 1;
                continue;
            }
            break;
        }

        // The statement's first word: LABEL if eligible and not a keyword,
        // otherwise OP.
        let w1_start = i;
        while i < n && is_symbol_char(cs[i].1) {
            i += 1;
        }
        let w1_start_byte = cs[w1_start].0;
        let w1_end_byte = byte_at(&cs, i, line_len);
        let w1 = &line[w1_start_byte..w1_end_byte];

        let mut op_word: Option<&str> = None;
        if label_eligible && !is_keyword(w1) {
            spans.push(Span {
                start: w1_start_byte,
                end: w1_end_byte,
                kind: TokenKind::Label,
            });
            while i < n && is_blank(cs[i].1) {
                i += 1;
            }
            if i < n && is_symbol_char(cs[i].1) {
                let w2_start = i;
                while i < n && is_symbol_char(cs[i].1) {
                    i += 1;
                }
                let w2_start_byte = cs[w2_start].0;
                let w2_end_byte = byte_at(&cs, i, line_len);
                let w2 = &line[w2_start_byte..w2_end_byte];
                let kind = if is_keyword(w2) {
                    TokenKind::Keyword
                } else {
                    TokenKind::UnknownMnemonic
                };
                spans.push(Span {
                    start: w2_start_byte,
                    end: w2_end_byte,
                    kind,
                });
                op_word = Some(w2);
            }
        } else {
            let kind = if is_keyword(w1) {
                TokenKind::Keyword
            } else {
                TokenKind::UnknownMnemonic
            };
            spans.push(Span {
                start: w1_start_byte,
                end: w1_end_byte,
                kind,
            });
            op_word = Some(w1);
        }

        while i < n && is_blank(cs[i].1) {
            i += 1;
        }

        // EXPR: the field after OP, styling only strings, character
        // constants and registers. `ESPEC` takes no EXPR at all, and a `%`
        // where EXPR would otherwise start is an empty EXPR immediately
        // followed by a remark -- either way nothing is scanned here, and
        // the remark step below picks up at the same `i`.
        if op_word != Some("ESPEC") && !(i < n && cs[i].1 == '%') {
            let mut paren_depth = 0u32;
            'expr: while i < n {
                match cs[i].1 {
                    ';' => break 'expr,
                    '"' => {
                        let start_byte = cs[i].0;
                        i += 1;
                        let mut end = line_len;
                        while i < n {
                            if cs[i].1 == '"' {
                                end = byte_at(&cs, i + 1, line_len);
                                i += 1;
                                break;
                            }
                            i += 1;
                        }
                        spans.push(Span {
                            start: start_byte,
                            end,
                            kind: TokenKind::String,
                        });
                    }
                    '\'' => {
                        // A character constant is quote, one character
                        // (possibly a quote or a backslash -- 0.3.13 has no
                        // escape), quote: always exactly three characters.
                        if i + 2 < n && cs[i + 2].1 == '\'' {
                            spans.push(Span {
                                start: cs[i].0,
                                end: byte_at(&cs, i + 3, line_len),
                                kind: TokenKind::Char,
                            });
                            i += 3;
                        } else {
                            i += 1;
                        }
                    }
                    '(' => {
                        paren_depth += 1;
                        i += 1;
                    }
                    ')' => {
                        paren_depth = paren_depth.saturating_sub(1);
                        i += 1;
                    }
                    '$' => {
                        let start_byte = cs[i].0;
                        let mut j = i + 1;
                        while j < n && cs[j].1.is_ascii_digit() {
                            j += 1;
                        }
                        if j > i + 1 {
                            spans.push(Span {
                                start: start_byte,
                                end: byte_at(&cs, j, line_len),
                                kind: TokenKind::Register,
                            });
                            i = j;
                        } else {
                            i += 1;
                        }
                    }
                    c if is_blank(c) && paren_depth == 0 => {
                        let prev_is_comma = i > 0 && cs[i - 1].1 == ',';
                        let mut k = i;
                        while k < n && is_blank(cs[k].1) {
                            k += 1;
                        }
                        let next_is_comma = k < n && cs[k].1 == ',';
                        if prev_is_comma || next_is_comma {
                            i = k;
                        } else {
                            break 'expr;
                        }
                    }
                    _ => i += 1,
                }
            }
        }

        while i < n && is_blank(cs[i].1) {
            i += 1;
        }
        if i < n && cs[i].1 == ';' {
            i += 1;
            continue;
        }
        if i >= n {
            break;
        }

        // Remark: text up to `;` or the end of the line, whatever its first
        // character -- except one opening with `%`, which swallows any `;`
        // and runs to the end of the line instead.
        let remark_start = cs[i].0;
        if cs[i].1 == '%' {
            spans.push(Span {
                start: remark_start,
                end: line_len,
                kind: TokenKind::Comment,
            });
            break;
        }
        let mut j = i;
        while j < n && cs[j].1 != ';' {
            j += 1;
        }
        spans.push(Span {
            start: remark_start,
            end: byte_at(&cs, j, line_len),
            kind: TokenKind::Comment,
        });
        if j < n {
            i = j + 1;
        } else {
            i = n;
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

    fn assert_disjoint_and_ordered(line: &str, spans: &[Span]) {
        for pair in spans.windows(2) {
            assert!(
                pair[0].end <= pair[1].start,
                "overlapping spans in {line:?}: {spans:?}"
            );
        }
    }

    #[test]
    fn classifies_label_and_string_from_hello_world() {
        let line = r#"Text	BYTE	"Hello world!",10,0"#;
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
    fn char_literal_is_exactly_quote_one_character_quote() {
        // 0.3.13 retired the backslash escape: a character constant is
        // always quote, one character, quote -- '\n' (the old 4-character
        // escape) no longer forms one, but a bare backslash as the one
        // character does.
        let line = "\tSET\t$2,'\\'";
        let spans = classify(line);
        let char_span = spans
            .iter()
            .find(|s| s.kind == TokenKind::Char)
            .expect("char span");
        assert_eq!(span_text(line, char_span), "'\\'");

        let old_escape_line = "\tSET\t$2,'\\n'";
        let spans = classify(old_escape_line);
        assert!(
            !spans.iter().any(|s| s.kind == TokenKind::Char),
            "the old 4-character escape must not form a char span: {spans:?}"
        );
    }

    #[test]
    fn unterminated_char_literal_emits_no_char_span() {
        // No closing '\'' anywhere on the line: not a valid char_literal
        // per the grammar, so it must not be classified as one, and the
        // spans that are emitted must stay disjoint and in order (the
        // invariant `render_line` depends on). The assembler rejects this
        // line -- Main has no OP directly after it -- so IS reads as part
        // of the trailing remark, not as a keyword.
        let line = "Main' IS 3";
        let spans = classify(line);

        assert!(!spans.iter().any(|s| s.kind == TokenKind::Char));
        assert_disjoint_and_ordered(line, &spans);
        assert_eq!(
            spans
                .iter()
                .map(|s| (s.kind, span_text(line, s)))
                .collect::<Vec<_>>(),
            vec![(TokenKind::Label, "Main"), (TokenKind::Comment, "IS 3")]
        );
    }

    #[test]
    fn jmpb_is_a_recognized_keyword_after_a_label() {
        let line = "Main\tJMPB\tLoop";
        let spans = classify(line);
        assert_eq!(
            spans
                .iter()
                .find(|s| s.kind == TokenKind::Keyword)
                .map(|s| span_text(line, s)),
            Some("JMPB")
        );
        assert!(!spans.iter().any(|s| s.kind == TokenKind::UnknownMnemonic));
    }

    #[test]
    fn unrecognized_mnemonic_right_after_a_label_is_flagged() {
        let line = "Main\tADDD\t$1,$2,$3";
        let spans = classify(line);
        assert_eq!(
            spans
                .iter()
                .find(|s| s.kind == TokenKind::UnknownMnemonic)
                .map(|s| span_text(line, s)),
            Some("ADDD")
        );
    }

    #[test]
    fn unrecognized_word_deep_in_operand_position_is_unstyled() {
        let line = "Main\tSET\t$1,ADDD";
        let spans = classify(line);
        assert!(!spans.iter().any(|s| s.kind == TokenKind::UnknownMnemonic));
        assert!(!spans.iter().any(|s| span_text(line, s) == "ADDD"));
    }

    #[test]
    fn a_string_between_a_label_and_an_unrecognized_word_leaves_it_in_a_remark() {
        // The assembler rejects this line: LABEL, OP, EXPR, remark is a
        // fixed field order, and nothing but a word can fill OP. A string
        // directly after the label leaves OP empty, so EXPR takes over from
        // there and everything past it is a remark.
        let line = r#"Main "foo" ADDD"#;
        let spans = classify(line);
        assert_disjoint_and_ordered(line, &spans);
        assert_eq!(
            spans
                .iter()
                .map(|s| (s.kind, span_text(line, s)))
                .collect::<Vec<_>>(),
            vec![
                (TokenKind::Label, "Main"),
                (TokenKind::String, "\"foo\""),
                (TokenKind::Comment, "ADDD"),
            ]
        );
    }

    #[test]
    fn a_char_literal_between_a_label_and_an_unrecognized_word_leaves_it_in_a_remark() {
        let line = "Main 'a' ADDD";
        let spans = classify(line);
        assert_disjoint_and_ordered(line, &spans);
        assert_eq!(
            spans
                .iter()
                .map(|s| (s.kind, span_text(line, s)))
                .collect::<Vec<_>>(),
            vec![
                (TokenKind::Label, "Main"),
                (TokenKind::Char, "'a'"),
                (TokenKind::Comment, "ADDD"),
            ]
        );
    }

    #[test]
    fn a_register_between_a_label_and_an_unrecognized_word_leaves_it_in_a_remark() {
        let line = "Main $1 ADDD";
        let spans = classify(line);
        assert_disjoint_and_ordered(line, &spans);
        assert_eq!(
            spans
                .iter()
                .map(|s| (s.kind, span_text(line, s)))
                .collect::<Vec<_>>(),
            vec![
                (TokenKind::Label, "Main"),
                (TokenKind::Register, "$1"),
                (TokenKind::Comment, "ADDD"),
            ]
        );
    }

    #[test]
    fn mixs_compare_and_jump_mnemonics_are_neither_keywords_nor_valid_mmixal() {
        // checksmix 0.3.9 dropped `JE`/`JNE`/`JL`/`JG` from the grammar --
        // MIX's compare-and-jump mnemonics, never real MMIXAL, that had
        // entered as aliases of `BZ`/`BNZ`/`BN`/`BP`. KEYWORDS carried all
        // four past that removal, which defeated `UnknownMnemonic` styling
        // for source checksmix now rejects outright. Each case pins both
        // halves: the word is gone from KEYWORDS, and a program that uses
        // it genuinely fails to assemble -- so this fails if either half of
        // the fix is ever undone.
        const CASES: [(&str, &str); 4] = [
            ("JE", "\tLOC\t#100\nMain\tJE\t$1,Main\n"),
            ("JNE", "\tLOC\t#100\nMain\tJNE\t$1,Main\n"),
            ("JL", "\tLOC\t#100\nMain\tJL\t$1,Main\n"),
            ("JG", "\tLOC\t#100\nMain\tJG\t$1,Main\n"),
        ];

        for (word, source) in CASES {
            assert!(
                !KEYWORDS.contains(word),
                "{word} must not be in KEYWORDS -- checksmix 0.3.9 dropped it \
                 from the grammar"
            );
            assert!(
                crate::control::Control::new(source, "drift.mms").is_err(),
                "{word} must fail to assemble -- a program using it is not \
                 valid MMIXAL under checksmix 0.3.9"
            );
        }
    }

    struct HighlightCase {
        line: &'static str,
        spans: &'static [(TokenKind, &'static str)],
        defines_main: bool,
        assembles: bool,
    }

    /// One row per `playmmix-repin-checksmix-0.3.13.md` §8's highlight
    /// table, each measured against the checksmix 0.3.13 CLI. `BSPEC` and
    /// `ESPEC` are covered by `bspec_and_espec_are_keywords` below.
    const HIGHLIGHT_CASES: &[HighlightCase] = &[
        // H1
        HighlightCase {
            line: "Main\tSET\t$1,2 set it",
            spans: &[
                (TokenKind::Label, "Main"),
                (TokenKind::Keyword, "SET"),
                (TokenKind::Register, "$1"),
                (TokenKind::Comment, "set it"),
            ],
            defines_main: true,
            assembles: true,
        },
        // H2
        HighlightCase {
            line: "Main\tSETL\t$1,2 note % x; INCL $1,5",
            spans: &[
                (TokenKind::Label, "Main"),
                (TokenKind::Keyword, "SETL"),
                (TokenKind::Register, "$1"),
                (TokenKind::Comment, "note % x"),
                (TokenKind::Keyword, "INCL"),
                (TokenKind::Register, "$1"),
            ],
            defines_main: true,
            assembles: true,
        },
        // H3
        HighlightCase {
            line: "Main\tSETL\t$1,2 % x; INCL $1,5",
            spans: &[
                (TokenKind::Label, "Main"),
                (TokenKind::Keyword, "SETL"),
                (TokenKind::Register, "$1"),
                (TokenKind::Comment, "% x; INCL $1,5"),
            ],
            defines_main: true,
            assembles: true,
        },
        // H4
        HighlightCase {
            line: "\tSET\t$1, 5 remark!!!!",
            spans: &[
                (TokenKind::Keyword, "SET"),
                (TokenKind::Register, "$1"),
                (TokenKind::Comment, "remark!!!!"),
            ],
            defines_main: false,
            assembles: true,
        },
        // H5
        HighlightCase {
            line: "\tset\t$1,2",
            spans: &[
                (TokenKind::UnknownMnemonic, "set"),
                (TokenKind::Register, "$1"),
            ],
            defines_main: false,
            assembles: false,
        },
        // H6
        HighlightCase {
            line: "Main\tset\t$1,2",
            spans: &[
                (TokenKind::Label, "Main"),
                (TokenKind::UnknownMnemonic, "set"),
                (TokenKind::Register, "$1"),
            ],
            defines_main: true,
            assembles: false,
        },
        // H7
        HighlightCase {
            line: "* note; INCL $1,5",
            spans: &[(TokenKind::Comment, "* note; INCL $1,5")],
            defines_main: false,
            assembles: true,
        },
        // H8
        HighlightCase {
            line: "2H\tSET\t$1,2",
            spans: &[
                (TokenKind::Label, "2H"),
                (TokenKind::Keyword, "SET"),
                (TokenKind::Register, "$1"),
            ],
            defines_main: false,
            assembles: true,
        },
        // H9
        HighlightCase {
            line: ":Main\tSET\t$1,2",
            spans: &[
                (TokenKind::Label, ":Main"),
                (TokenKind::Keyword, "SET"),
                (TokenKind::Register, "$1"),
            ],
            defines_main: true,
            assembles: true,
        },
        // H10
        HighlightCase {
            line: "Main\tSET\t$1,'''",
            spans: &[
                (TokenKind::Label, "Main"),
                (TokenKind::Keyword, "SET"),
                (TokenKind::Register, "$1"),
                (TokenKind::Char, "'''"),
            ],
            defines_main: true,
            assembles: true,
        },
        // H11
        HighlightCase {
            line: "\tSET\t$2,'\\'",
            spans: &[
                (TokenKind::Keyword, "SET"),
                (TokenKind::Register, "$2"),
                (TokenKind::Char, "'\\'"),
            ],
            defines_main: false,
            assembles: true,
        },
        // H12
        HighlightCase {
            line: "\tLOCAL\t$40",
            spans: &[(TokenKind::Keyword, "LOCAL"), (TokenKind::Register, "$40")],
            defines_main: false,
            assembles: true,
        },
        // H13
        HighlightCase {
            line: "\t.BYTE\t1,2,3",
            spans: &[(TokenKind::Comment, ".BYTE\t1,2,3")],
            defines_main: false,
            assembles: true,
        },
        // H14
        HighlightCase {
            line: "Main\tdebug \"hi\"",
            spans: &[
                (TokenKind::Label, "Main"),
                (TokenKind::Keyword, "debug"),
                (TokenKind::String, "\"hi\""),
            ],
            defines_main: true,
            assembles: true,
        },
        // H15
        HighlightCase {
            line: "Main\tDEBUG \"hi\"",
            spans: &[
                (TokenKind::Label, "Main"),
                (TokenKind::UnknownMnemonic, "DEBUG"),
                (TokenKind::String, "\"hi\""),
            ],
            defines_main: true,
            assembles: false,
        },
        // H16
        HighlightCase {
            line: "Main\tSETL\t$1,2;Foo INCL $1,5",
            spans: &[
                (TokenKind::Label, "Main"),
                (TokenKind::Keyword, "SETL"),
                (TokenKind::Register, "$1"),
                (TokenKind::Label, "Foo"),
                (TokenKind::Keyword, "INCL"),
                (TokenKind::Register, "$1"),
            ],
            defines_main: true,
            assembles: true,
        },
        // H17
        HighlightCase {
            line: "\tBYTE\t\";\",1",
            spans: &[(TokenKind::Keyword, "BYTE"), (TokenKind::String, "\";\"")],
            defines_main: false,
            assembles: true,
        },
        // H18
        HighlightCase {
            line: "Main\tSET\t$1,7%4",
            spans: &[
                (TokenKind::Label, "Main"),
                (TokenKind::Keyword, "SET"),
                (TokenKind::Register, "$1"),
            ],
            defines_main: true,
            assembles: true,
        },
        // H19
        HighlightCase {
            line: "X\tBYTE\t\"100%\"\t% real comment",
            spans: &[
                (TokenKind::Label, "X"),
                (TokenKind::Keyword, "BYTE"),
                (TokenKind::String, "\"100%\""),
                (TokenKind::Comment, "% real comment"),
            ],
            defines_main: false,
            assembles: true,
        },
        // H20
        HighlightCase {
            line: "Main\tSETL\t$1,2 #zz; INCL $1,5",
            spans: &[
                (TokenKind::Label, "Main"),
                (TokenKind::Keyword, "SETL"),
                (TokenKind::Register, "$1"),
                (TokenKind::Comment, "#zz"),
                (TokenKind::Keyword, "INCL"),
                (TokenKind::Register, "$1"),
            ],
            defines_main: true,
            assembles: true,
        },
        // H21: a `%` where EXPR would start is an empty EXPR immediately
        // followed by a remark, so the `;` and everything past it are dead
        // text, not a second statement.
        HighlightCase {
            line: "\tSWYM\t% note; INCL $1,5",
            spans: &[
                (TokenKind::Keyword, "SWYM"),
                (TokenKind::Comment, "% note; INCL $1,5"),
            ],
            defines_main: false,
            assembles: true,
        },
        // H22
        HighlightCase {
            line: "Foo\t% entry; INCL $1,5",
            spans: &[
                (TokenKind::Label, "Foo"),
                (TokenKind::Comment, "% entry; INCL $1,5"),
            ],
            defines_main: false,
            assembles: true,
        },
        // H23: LABEL with no OP word at all -- the rest of the line is a
        // remark from the very first character after the label.
        HighlightCase {
            line: "Main\t% entry point",
            spans: &[
                (TokenKind::Label, "Main"),
                (TokenKind::Comment, "% entry point"),
            ],
            defines_main: true,
            assembles: true,
        },
    ];

    /// Each row asserts its spans, in order, and its Assembles cell through
    /// `Control::new`, wrapping the line exactly as the dispatch prompt's
    /// §8 specifies.
    #[test]
    fn highlight_table_matches_checksmix_0_3_13() {
        for case in HIGHLIGHT_CASES {
            let spans = classify(case.line);
            assert_disjoint_and_ordered(case.line, &spans);
            let actual: Vec<(TokenKind, &str)> = spans
                .iter()
                .map(|s| (s.kind, span_text(case.line, s)))
                .collect();
            assert_eq!(actual, case.spans, "line: {:?}", case.line);

            let prefix = if case.defines_main {
                ""
            } else {
                "Main\tSWYM\n"
            };
            let source = format!("\tLOC\t#100\n{prefix}{}\n\tTRAP\t0,Halt,0\n", case.line);
            let assembles = crate::control::Control::new(&source, "highlight.mms").is_ok();
            assert_eq!(
                assembles, case.assembles,
                "assembles mismatch for line {:?}\nsource:\n{source}",
                case.line
            );
        }
    }

    #[test]
    fn bspec_and_espec_are_keywords() {
        let bspec_source = "\tLOC\t#100\nMain\tSWYM\n\tBSPEC\t5\n\tESPEC\n\tTRAP\t0,Halt,0\n";
        assert!(
            crate::control::Control::new(bspec_source, "bspec.mms").is_ok(),
            "BSPEC 5 / ESPEC must assemble"
        );

        let bspec_line = "\tBSPEC\t5";
        let spans = classify(bspec_line);
        assert_eq!(
            spans
                .iter()
                .find(|s| s.kind == TokenKind::Keyword)
                .map(|s| span_text(bspec_line, s)),
            Some("BSPEC")
        );

        // ESPEC takes no EXPR: text after it is a remark. A bare ESPEC
        // needs a matching BSPEC in scope to assemble at all -- an
        // assembler-level check, not a grammar one, so the fixture opens
        // one first.
        let espec_line = "ESPEC close it";
        let espec_source =
            format!("\tLOC\t#100\nMain\tSWYM\n\tBSPEC\t5\n{espec_line}\n\tTRAP\t0,Halt,0\n");
        assert!(
            crate::control::Control::new(&espec_source, "espec.mms").is_ok(),
            "ESPEC close it must assemble after a matching BSPEC"
        );
        let spans = classify(espec_line);
        assert_disjoint_and_ordered(espec_line, &spans);
        assert_eq!(
            spans
                .iter()
                .map(|s| (s.kind, span_text(espec_line, s)))
                .collect::<Vec<_>>(),
            vec![
                (TokenKind::Keyword, "ESPEC"),
                (TokenKind::Comment, "close it"),
            ]
        );
    }
}
