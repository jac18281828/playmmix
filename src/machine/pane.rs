//! The machine pane: registers, special registers, and memory rendering.

use std::collections::BTreeSet;

use yew::prelude::*;

use crate::machine::memory::{MemoryRow, memory_row_instruction_span, memory_row_is_current};
use crate::machine::registers::{RegisterClass, RegisterRow, SpecialRegisterRow};

#[derive(Properties, PartialEq)]
pub struct MachinePaneProps {
    pub registers: Vec<RegisterRow>,
    pub specials: Vec<SpecialRegisterRow>,
    pub memory: Vec<MemoryRow>,
    pub pc: u64,
    /// "Where you are": `get_pc()` while running/paused, the address of the
    /// instruction that halted once halted -- see `Control::marker_pc`.
    /// Drives the memory pane's current-row and current-instruction
    /// highlights.
    pub marker_pc: u64,
    /// The exit code from `TRAP 0,Halt,0`, meaningful only once the
    /// machine has halted.
    pub exit_code: Option<u64>,
    pub call_depth: usize,
    /// Registers/specials/memory addresses whose value differs from the
    /// previous paused render -- empty while running, per
    /// `docs/layout-spec.md`'s Highlights §3.
    pub changed_registers: BTreeSet<u8>,
    pub changed_specials: BTreeSet<String>,
    pub changed_memory: BTreeSet<u64>,
}

/// The machine pane: registers, special registers, and memory, computed
/// fresh from the current machine state on every render (no deltas of its
/// own -- `changed_*` is computed by `App` and passed in already).
/// Registers and specials share one scroll region (specials render
/// immediately under registers, per this prompt's deviation from a literal
/// reading of the layout spec's three-pane scroll list); memory scrolls
/// independently.
#[function_component(MachinePane)]
pub fn machine_pane(props: &MachinePaneProps) -> Html {
    html! {
        <div class="machine-pane">
            <div class="machine-status">
                <span title="Program counter: the address of the next instruction to execute.">{ format!("PC 0x{:016X}", props.pc) }</span>
                <span title="How many nested subroutine calls (PUSHJ) are active. Each call gets its own window onto the local registers -- $0 inside a call is not the same storage as $0 before it, by design.">{ format!("call depth {}", props.call_depth) }</span>
                { for props.exit_code.map(|code| html! { <span title="The value TRAP 0,Halt,0 reads from $255 when the program halts -- not from the instruction's own written operand.">{ format!("exit {code}") }</span> }) }
            </div>
            <div class="registers-scroll">
                <section class="registers">
                    <h2 title="General-purpose registers. $0 up to rL are local to the current call frame; rL up to rG are marginal -- read as zero, and writing one raises rL to claim it; rG upward are global.">{ "Registers" }</h2>
                    <div class="register-grid">
                        { for props.registers.iter().map(|row| render_register_row(row, &props.changed_registers)) }
                    </div>
                </section>
                <section class="specials">
                    <h2 title="CPU state registers -- see each one's own name below.">{ "Special registers" }</h2>
                    <div class="register-grid">
                        { for props.specials.iter().map(|row| render_special_row(row, &props.changed_specials)) }
                    </div>
                </section>
            </div>
            <section class="memory">
                <h2>{ "Memory" }</h2>
                <div class="memory-grid">
                    { for props.memory.iter().map(|row| render_memory_row(row, props.marker_pc, &props.changed_memory)) }
                </div>
            </section>
        </div>
    }
}

/// The `global · rG={rg}` caption cell, per `docs/layout-spec.md`'s
/// Registers section. Plain and `String`-returning so it is testable
/// without rendering `Html`.
pub(super) fn global_boundary_note(rg: u64) -> String {
    format!("global \u{b7} rG={rg}")
}

/// The decimal cell's text and optional title, as [`decimal_cell`] reads
/// them from a register's raw bits.
pub(super) struct DecimalCell {
    pub(super) text: String,
    pub(super) title: Option<String>,
}

/// Reads `value`'s bits as a float when its biased exponent field, `E =
/// (value >> 52) & 0x7FF`, falls in `923..=1123` -- an unbiased exponent in
/// `-100..=100`, magnitudes from 2^-100 (about 7.9e-31) up to 2^101 (about
/// 2.5e30) -- or when it is one of the two infinities, `E = 0x7FF` with a
/// zero mantissa. The window itself excludes zero, subnormals and every
/// ordinary integer a program holds: small positive integers have `E = 0`,
/// small negatives have `E = 0x7FF`, and segment addresses land far outside
/// it (`Data_Segment` near 1e-154, `Stack_Segment` near 1e154). Three
/// misreadings are accepted: a `Pool_Segment` pointer, `#4000000000000000`,
/// reads as `(2.0)`, and the infinities' patterns read the integers
/// 9218868437227405312 and -2^52 as `(inf)` and `(-inf)`. NaN (`E = 0x7FF`, mantissa nonzero) stays an integer:
/// every integer from -1 to -(2^52 - 1) has NaN bits, so reading NaN would
/// misread that whole range.
///
/// A float reading never prints like an integer: a finite value's `{:?}`
/// carries a decimal point or an exponent and every digit needed to
/// round-trip, and an infinity prints as `inf` or `-inf`. Its title gives
/// the value as a signed 64-bit integer, so a random word misread as a
/// float can be checked. An integer reading carries no title.
pub(super) fn decimal_cell(value: u64) -> DecimalCell {
    let exponent = (value >> 52) & 0x7FF;
    let is_infinite = exponent == 0x7FF && value & 0x000F_FFFF_FFFF_FFFF == 0;
    if (923..=1123).contains(&exponent) || is_infinite {
        DecimalCell {
            text: format!("({:?})", f64::from_bits(value)),
            title: Some(format!("as an integer: {}", value as i64)),
        }
    } else {
        DecimalCell {
            text: format!("({})", value as i64),
            title: None,
        }
    }
}

/// A general-register row's name, hex and decimal cell text, in render
/// order -- the seam `render_register_row` renders and host tests read
/// without a browser. The decimal cell is [`decimal_cell`]'s text: the
/// signed integer, or the float reading when the bits are float-shaped,
/// inside parentheses directly after the hex, per
/// `docs/layout-spec.md`'s Registers section.
pub(super) fn register_row_cells(index: u8, value: u64) -> [String; 3] {
    [
        format!("${index}"),
        format!("0x{value:016X}"),
        decimal_cell(value).text,
    ]
}

/// A special-register row's name, hex and decimal cell text, in render
/// order -- the same shared format [`register_row_cells`] renders for
/// general registers.
pub(super) fn special_row_cells(name: &str, value: u64) -> [String; 3] {
    [
        name.to_string(),
        format!("0x{value:016X}"),
        decimal_cell(value).text,
    ]
}

/// The register-row spans a change of class must never add or remove: fixed
/// widths in `style.css` reserve their space whether or not this row uses
/// them, so a row's span sequence and `ch` layout stay identical however
/// `rL`/`rG` move -- the no-reflow invariant `docs/layout-spec.md` requires.
fn render_register_row(row: &RegisterRow, changed: &BTreeSet<u8>) -> Html {
    match row {
        RegisterRow::Register {
            index,
            value,
            class,
        } => {
            let is_changed = changed.contains(index);
            let mut row_class = classes!("register-row");
            if *class == RegisterClass::Marginal {
                row_class.push("register-marginal");
            }
            let [name_text, hex_text, dec_text] = register_row_cells(*index, *value);
            let dec_title = decimal_cell(*value).title;
            let mut hex_class = classes!("reg-hex");
            let mut dec_class = classes!("reg-dec");
            if is_changed {
                hex_class.push("changed");
                dec_class.push("changed");
            }
            html! {
                <div class={row_class}>
                    <span class="reg-name">{ name_text }</span>
                    <span class={hex_class}>{ hex_text }</span>
                    <span class={dec_class} title={dec_title}>{ dec_text }</span>
                </div>
            }
        }
        RegisterRow::GlobalBoundary { rg } => {
            html! {
                <div class="register-row register-boundary">
                    <span class="reg-boundary-text">{ global_boundary_note(*rg) }</span>
                </div>
            }
        }
    }
}

/// Hover text for each of the six
/// [`crate::machine::registers::PINNED_SPECIALS`], keyed by their `rX`
/// display name. `None` for any other special (`visible_specials` only ever
/// adds a name here for a pinned one, so this never needs to answer for the
/// rest of the ~30 MMIX specials).
fn pinned_special_title(name: &str) -> Option<&'static str> {
    match name {
        "rA" => Some("Arithmetic status: sticky exception flags."),
        "rG" => Some(
            "Global threshold: registers at or above this number are global, not local to a call.",
        ),
        "rL" => Some("Local register count: how many of the current frame's registers are in use."),
        "rO" => Some("Register stack offset."),
        "rS" => Some("Register stack pointer."),
        "rJ" => Some("Return address: where the matching POP will jump to."),
        _ => None,
    }
}

fn render_special_row(row: &SpecialRegisterRow, changed: &BTreeSet<String>) -> Html {
    let is_changed = changed.contains(&row.name);
    let mut hex_class = classes!("reg-hex");
    let mut dec_class = classes!("reg-dec");
    if is_changed {
        hex_class.push("changed");
        dec_class.push("changed");
    }
    let title = pinned_special_title(&row.name);
    let [name_text, hex_text, dec_text] = special_row_cells(&row.name, row.value);
    let dec_title = decimal_cell(row.value).title;
    html! {
        <div class="register-row">
            <span class="reg-name" title={title}>{ name_text }</span>
            <span class={hex_class}>{ hex_text }</span>
            <span class={dec_class} title={dec_title}>{ dec_text }</span>
        </div>
    }
}

fn render_memory_row(row: &MemoryRow, marker_pc: u64, changed: &BTreeSet<u64>) -> Html {
    let MemoryRow::Data {
        segment,
        addr,
        cells,
        labels,
    } = row
    else {
        return html! { <div class="memory-separator"></div> };
    };

    let is_current = memory_row_is_current(row, marker_pc);
    let instruction_span = memory_row_instruction_span(row, marker_pc);

    let hex_cells: Html = cells
        .iter()
        .enumerate()
        .map(|(offset, cell)| {
            let byte_addr = addr + offset as u64;
            let mut class = classes!("mem-byte");
            if instruction_span.contains(&offset) {
                class.push("mem-current-instruction");
            }
            if changed.contains(&byte_addr) {
                class.push("changed");
            }
            let text = match cell {
                Some(byte) => format!("{byte:02x}"),
                None => String::from("  "),
            };
            html! { <span {class}>{ text }</span> }
        })
        .collect();

    let ascii: String = cells
        .iter()
        .map(|cell| match cell {
            Some(byte) if (0x20..=0x7e).contains(byte) => *byte as char,
            Some(_) => '.',
            None => ' ',
        })
        .collect();

    let mut row_class = classes!("memory-row");
    if is_current {
        row_class.push("mem-current");
    }

    html! {
        <div class={row_class}>
            <span class="mem-segment">{ segment.label() }</span>
            <span class="mem-addr">{ format!("0x{addr:016X}") }</span>
            <span class="mem-hex">{ hex_cells }</span>
            <span class="mem-ascii">{ ascii }</span>
            <span class="mem-label">{ labels.join(", ") }</span>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::machine::registers::{PINNED_SPECIALS, special_reg_name};

    #[test]
    fn every_pinned_special_has_a_hover_title_keyed_by_its_display_name() {
        for reg in PINNED_SPECIALS {
            let name = special_reg_name(reg);
            assert!(
                pinned_special_title(&name).is_some(),
                "pinned_special_title has no entry for {name:?} ({reg:?}) -- \
                 a rename of special_reg_name's output would silently drop \
                 this register's tooltip with no compile error"
            );
        }
    }

    #[test]
    fn register_row_cells_return_name_hex_and_decimal_in_render_order() {
        assert_eq!(
            register_row_cells(1, 2),
            [
                "$1".to_string(),
                "0x0000000000000002".to_string(),
                "(2)".to_string(),
            ]
        );
        assert_eq!(
            register_row_cells(255, u64::MAX),
            [
                "$255".to_string(),
                "0xFFFFFFFFFFFFFFFF".to_string(),
                "(-1)".to_string(),
            ]
        );
    }

    #[test]
    fn special_row_cells_return_name_hex_and_decimal_in_render_order() {
        assert_eq!(
            special_row_cells("rL", 1),
            [
                "rL".to_string(),
                "0x0000000000000001".to_string(),
                "(1)".to_string(),
            ]
        );
    }

    #[test]
    fn decimal_cell_shows_the_signed_integer_or_the_float_reading_the_bits_hold() {
        // Window edges: E = 923 and E = 1123 read as floats, E = 922 and
        // E = 1124 as integers. The infinities also read as floats. Zero,
        // subnormals, NaN and the segment addresses stay integers.
        let cases: &[(u64, &str, Option<&str>)] = &[
            (
                0x3FE0000000000000,
                "(0.5)",
                Some("as an integer: 4602678819172646912"),
            ),
            (
                0x4000000000000000,
                "(2.0)",
                Some("as an integer: 4611686018427387904"),
            ),
            (
                0xBFE0000000000000,
                "(-0.5)",
                Some("as an integer: -4620693217682128896"),
            ),
            (
                0x3E112E0BE826D695,
                "(1e-9)",
                Some("as an integer: 4472406533629990549"),
            ),
            (
                0x44DFDE9F10A8D361,
                "(6.02e23)",
                Some("as an integer: 4962930089146241889"),
            ),
            (
                0x3FD3333333333334,
                "(0.30000000000000004)",
                Some("as an integer: 4599075939470750516"),
            ),
            (
                0x39B0000000000000,
                "(7.888609052210118e-31)",
                Some("as an integer: 4156822456062967808"),
            ),
            (0x39A0000000000000, "(4152318856435597312)", None),
            (
                0x4630000000000000,
                "(1.2676506002282294e30)",
                Some("as an integer: 5057542381537067008"),
            ),
            (0x4640000000000000, "(5062045981164437504)", None),
            (0x0000000000000000, "(0)", None),
            (0x0000000000000001, "(1)", None),
            (0x000000001DCD6500, "(500000000)", None),
            (0xFFFFFFFFFFFFFFFF, "(-1)", None),
            (0x2000000000000000, "(2305843009213693952)", None),
            (0x6000000000000000, "(6917529027641081856)", None),
            (0x8000000000000000, "(-9223372036854775808)", None),
            (
                0x7FF0000000000000,
                "(inf)",
                Some("as an integer: 9218868437227405312"),
            ),
            (
                0xFFF0000000000000,
                "(-inf)",
                Some("as an integer: -4503599627370496"),
            ),
            (0x7FF8000000000000, "(9221120237041090560)", None),
            (0xFFF8000000000000, "(-2251799813685248)", None),
            (0x7FF0000000000001, "(9218868437227405313)", None),
            (0xFFF0000000000001, "(-4503599627370495)", None),
        ];

        for (bits, text, title) in cases {
            let cell = decimal_cell(*bits);
            assert_eq!(&cell.text, text, "bits {bits:#018X}");
            assert_eq!(cell.title.as_deref(), *title, "bits {bits:#018X}");
        }
    }

    #[test]
    fn a_float_shaped_value_reads_the_same_through_both_row_seams() {
        assert_eq!(register_row_cells(1, 0x3FE0000000000000)[2], "(0.5)");
        assert_eq!(special_row_cells("rE", 0x3FE0000000000000)[2], "(0.5)");
    }
}
