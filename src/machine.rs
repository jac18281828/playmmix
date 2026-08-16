//! Machine pane: general registers, special registers, and loaded memory.
//!
//! Computation is plain functions over `&MMix` and the assembler's label
//! table (`AGENTS.md`'s rule that logic not needing browser APIs stays
//! host-testable); [`MachinePane`] only renders their *owned* output --
//! `Properties` must be `'static`, so a borrowed `&MMix` can't cross that
//! boundary.

use std::collections::HashMap;

use checksmix::{MMix, SpecialReg};
use yew::prelude::*;

/// `rG`'s value with no `GREG` directive at all: `MMix::initialize`'s
/// default. `write_image` only ever raises `rG` above this floor, and only
/// when a program uses `GREG`, so this value uniquely identifies (modulo
/// the documented edge case where the lowest `GREG` lands exactly on `$32`)
/// a program with no global register allocated at all.
const NO_GREG_RG: u64 = 32;

/// The six special registers always shown, regardless of value -- how the
/// register stack is taught.
const PINNED_SPECIALS: [SpecialReg; 6] = [
    SpecialReg::RA,
    SpecialReg::RG,
    SpecialReg::RL,
    SpecialReg::RO,
    SpecialReg::RS,
    SpecialReg::RJ,
];

/// Bytes shown per memory row: wide enough to read a short string at a
/// glance, narrow enough to fit one line.
const MEMORY_ROW_WIDTH: usize = 16;

/// One row of the visible general-register table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterRow {
    /// Register `$index`, individually visible under the full ISA rule.
    Register { index: u8, value: u64 },
    /// `$32..=$255`, collapsed into one row: no `GREG` directive ran, so
    /// every register in this range is genuinely unallocated (never gated
    /// on value -- `Control` has no way to tell an allocated-but-zero
    /// global from a never-allocated one).
    UnallocatedGlobalRange,
}

/// Visible general registers under the full ISA rule: show `$i` when its
/// value is nonzero, or `i < rL` (a local register in use), or `i >= rG` (a
/// global register) -- ascending order. Collapses `$32..=$255` into one
/// summary row when `rG` still holds `initialize()`'s default, since that
/// only happens when no `GREG` directive ran and the whole range is then
/// genuinely unallocated.
pub fn visible_registers(mmix: &MMix) -> Vec<RegisterRow> {
    let rg = mmix.get_special(SpecialReg::RG);
    let rl = mmix.get_special(SpecialReg::RL);
    let mut rows = Vec::new();
    for i in 0u16..256 {
        let addr = u64::from(i);
        if rg == NO_GREG_RG && addr >= NO_GREG_RG {
            rows.push(RegisterRow::UnallocatedGlobalRange);
            break;
        }
        let index = i as u8;
        let value = mmix.get_register(index);
        if value != 0 || addr < rl || addr >= rg {
            rows.push(RegisterRow::Register { index, value });
        }
    }
    rows
}

/// One row of the special-register table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialRegisterRow {
    pub name: String,
    pub value: u64,
}

/// `SpecialReg`'s name, lowercased on the leading `R` only (`"RG"` ->
/// `"rG"`), matching the pane's `rA`/`rG`/... convention. Derived from
/// `SpecialReg`'s own `Debug` impl rather than a positional array indexed
/// by `reg as usize` -- checksmix's own `Display` impl does that and gets
/// it wrong (index 19 prints `"rT"` where the real register is `rG`).
fn special_reg_name(reg: SpecialReg) -> String {
    let debug = format!("{reg:?}");
    match debug.strip_prefix('R') {
        Some(rest) => format!("r{rest}"),
        None => debug,
    }
}

/// The six pinned special registers, always shown, plus any other nonzero
/// one.
pub fn visible_specials(mmix: &MMix) -> Vec<SpecialRegisterRow> {
    let mut rows: Vec<SpecialRegisterRow> = PINNED_SPECIALS
        .iter()
        .map(|&reg| SpecialRegisterRow {
            name: special_reg_name(reg),
            value: mmix.get_special(reg),
        })
        .collect();

    for n in 0u8..32 {
        let Some(reg) = SpecialReg::from_u8(n) else {
            continue;
        };
        if PINNED_SPECIALS.contains(&reg) {
            continue;
        }
        let value = mmix.get_special(reg);
        if value != 0 {
            rows.push(SpecialRegisterRow {
                name: special_reg_name(reg),
                value,
            });
        }
    }
    rows
}

/// One of MMIX's four segments, selected by an address's top three bits.
/// checksmix doesn't export this constant (`control.rs` restates
/// `DATA_SEGMENT_START` the same way); it's a stable MMIX architectural
/// boundary, safe to restate here too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    Text,
    Data,
    Pool,
    Stack,
}

impl Segment {
    fn from_addr(addr: u64) -> Self {
        match addr >> 61 {
            0 => Segment::Text,
            1 => Segment::Data,
            2 => Segment::Pool,
            _ => Segment::Stack,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Segment::Text => "text",
            Segment::Data => "data",
            Segment::Pool => "pool",
            Segment::Stack => "stack",
        }
    }
}

/// One contiguous run of loaded memory within a single segment, tagged
/// with any labels naming an address inside it.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRun {
    pub segment: Segment,
    pub start: u64,
    pub bytes: Vec<u8>,
    /// Labels landing inside this run, in ascending address order.
    pub labels: Vec<(u64, String)>,
}

/// `loaded_extent()`'s addresses -- what `write_image` loaded, including a
/// byte the program set to zero, which `occupied()`'s sparse-memory view
/// would drop -- collapsed into contiguous per-segment runs and tagged
/// with `labels` entries landing inside them.
pub fn memory_runs(mmix: &MMix, labels: &HashMap<String, u64>) -> Vec<MemoryRun> {
    let mut runs: Vec<MemoryRun> = Vec::new();

    for (addr, byte) in mmix.loaded_extent() {
        let segment = Segment::from_addr(addr);
        let extends = runs.last().is_some_and(|run| {
            run.segment == segment && run.start + run.bytes.len() as u64 == addr
        });
        if extends {
            runs.last_mut()
                .expect("just checked non-empty")
                .bytes
                .push(byte);
        } else {
            runs.push(MemoryRun {
                segment,
                start: addr,
                bytes: vec![byte],
                labels: Vec::new(),
            });
        }
    }

    for (name, &addr) in labels {
        if let Some(run) = runs
            .iter_mut()
            .find(|run| addr >= run.start && addr < run.start + run.bytes.len() as u64)
        {
            run.labels.push((addr, name.clone()));
        }
    }
    for run in &mut runs {
        run.labels.sort();
    }

    runs
}

/// One displayed row: a fixed-width slice of a [`MemoryRun`], with the
/// labels landing inside that slice.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRow {
    pub segment: Segment,
    pub addr: u64,
    pub bytes: Vec<u8>,
    pub labels: Vec<String>,
}

/// Chunk each run into fixed-width display rows.
pub fn memory_rows(runs: &[MemoryRun]) -> Vec<MemoryRow> {
    let mut rows = Vec::new();
    for run in runs {
        for (chunk_index, chunk) in run.bytes.chunks(MEMORY_ROW_WIDTH).enumerate() {
            let row_start = run.start + (chunk_index * MEMORY_ROW_WIDTH) as u64;
            let row_end = row_start + chunk.len() as u64;
            let labels = run
                .labels
                .iter()
                .filter(|(addr, _)| *addr >= row_start && *addr < row_end)
                .map(|(_, name)| name.clone())
                .collect();
            rows.push(MemoryRow {
                segment: run.segment,
                addr: row_start,
                bytes: chunk.to_vec(),
                labels,
            });
        }
    }
    rows
}

/// A byte's ASCII column rendering: printable as itself, else `.`.
fn ascii_column(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if (0x20..=0x7e).contains(&b) {
                b as char
            } else {
                '.'
            }
        })
        .collect()
}

#[derive(Properties, PartialEq)]
pub struct MachinePaneProps {
    pub registers: Vec<RegisterRow>,
    pub specials: Vec<SpecialRegisterRow>,
    pub memory: Vec<MemoryRow>,
    pub pc: u64,
    /// The exit code from `TRAP 0,Halt,0`, meaningful only once the
    /// machine has halted.
    pub exit_code: Option<u64>,
    pub call_depth: usize,
}

/// The machine pane: registers, special registers, and memory, computed
/// fresh from the current machine state on every render (no deltas -- see
/// the dispatch prompt's Scope).
#[function_component(MachinePane)]
pub fn machine_pane(props: &MachinePaneProps) -> Html {
    html! {
        <div class="machine-pane">
            <div class="machine-status">
                <span>{ format!("PC 0x{:016X}", props.pc) }</span>
                <span>{ format!("call depth {}", props.call_depth) }</span>
                { for props.exit_code.map(|code| html! { <span>{ format!("exit {code}") }</span> }) }
            </div>
            <section class="registers">
                <h2>{ "Registers" }</h2>
                <div class="register-grid">
                    { for props.registers.iter().map(render_register_row) }
                </div>
            </section>
            <section class="specials">
                <h2>{ "Special registers" }</h2>
                <div class="register-grid">
                    { for props.specials.iter().map(render_special_row) }
                </div>
            </section>
            <section class="memory">
                <h2>{ "Memory" }</h2>
                <div class="memory-grid">
                    { for props.memory.iter().map(render_memory_row) }
                </div>
            </section>
        </div>
    }
}

fn render_register_row(row: &RegisterRow) -> Html {
    match row {
        RegisterRow::Register { index, value } => html! {
            <div class="register-row">
                <span class="reg-name">{ format!("${index}") }</span>
                <span class="reg-hex">{ format!("0x{value:016X}") }</span>
                <span class="reg-dec">{ (*value as i64).to_string() }</span>
            </div>
        },
        RegisterRow::UnallocatedGlobalRange => html! {
            <div class="register-row register-collapsed">
                <span class="reg-name">{ "$32\u{2013}$255" }</span>
                <span class="reg-note">{ "unallocated (0)" }</span>
            </div>
        },
    }
}

fn render_special_row(row: &SpecialRegisterRow) -> Html {
    html! {
        <div class="register-row">
            <span class="reg-name">{ &row.name }</span>
            <span class="reg-hex">{ format!("0x{:016X}", row.value) }</span>
            <span class="reg-dec">{ (row.value as i64).to_string() }</span>
        </div>
    }
}

fn render_memory_row(row: &MemoryRow) -> Html {
    let hex: String = row.bytes.iter().map(|b| format!("{b:02x} ")).collect();
    let ascii = ascii_column(&row.bytes);
    let label = row.labels.join(", ");
    html! {
        <div class="memory-row">
            <span class="mem-segment">{ row.segment.label() }</span>
            <span class="mem-addr">{ format!("0x{:016X}", row.addr) }</span>
            <span class="mem-hex">{ hex }</span>
            <span class="mem-ascii">{ ascii }</span>
            <span class="mem-label">{ label }</span>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use checksmix::{MMixAssembler, entry_point, write_image};

    /// Assemble `source` and load it, unexecuted -- the same shape
    /// `Control::assemble_and_load` uses, restated here so these tests
    /// don't need a `Control`.
    fn assemble(source: &str, filename: &str) -> MMix {
        let mut assembler = MMixAssembler::new(source, filename);
        assembler.parse().expect("test program assembles");
        let mut mmix = MMix::new();
        write_image(&mut mmix, &assembler);
        mmix.set_pc(entry_point(&assembler));
        mmix
    }

    /// Two `GREG`s, one initialized to a literal zero -- verified against
    /// checksmix `main` while authoring the dispatch prompt: `rG = 253`,
    /// `rL = 0`. `$254` (from `G1 GREG 0`) and `$255` (never allocated) are
    /// both zero but must still show, because clause 3 (`i >= rG`) marks
    /// them global regardless of value.
    const TWO_GREG_MMS: &str = "\tLOC\t#100\nG1\tGREG\t0\nG2\tGREG\t@\nMain\tTRAP\t0,Halt,0\n";

    #[test]
    fn visible_registers_include_allocated_zero_globals_via_i_ge_rg() {
        let mmix = assemble(TWO_GREG_MMS, "two_greg.mms");
        assert_eq!(
            mmix.get_special(SpecialReg::RG),
            253,
            "fixture must allocate two GREGs starting at $253"
        );
        assert_eq!(mmix.get_special(SpecialReg::RL), 0);

        let indices: Vec<u8> = visible_registers(&mmix)
            .into_iter()
            .map(|row| match row {
                RegisterRow::Register { index, .. } => index,
                RegisterRow::UnallocatedGlobalRange => {
                    panic!("rG = 253, not 32; must not collapse")
                }
            })
            .collect();

        // Deleting the `i >= rG` clause would drop $254 and $255 (both
        // zero) from this set, leaving only $253 (G2's nonzero `@` value).
        assert_eq!(indices, vec![253, 254, 255]);
    }

    /// One local write with no `GREG` at all -- `SETL $1,5` grows `rL` to
    /// 2 (`MMix::set_register`'s doc comment), which must make `$0` --
    /// never written, value 0 -- visible too.
    const LOCAL_WRITE_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,5\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn visible_registers_include_untouched_locals_via_i_lt_rl() {
        let mut mmix = assemble(LOCAL_WRITE_MMS, "local.mms");
        assert!(mmix.execute_instruction(), "SETL must execute, not halt");
        assert_eq!(mmix.get_special(SpecialReg::RL), 2);

        // Deleting the `i < rL` clause would drop $0 from this set, since
        // its value is 0 and it is far below rG.
        let has_zero_reg0 = visible_registers(&mmix)
            .iter()
            .any(|row| matches!(row, RegisterRow::Register { index: 0, value: 0 }));
        assert!(has_zero_reg0, "$0 must be visible: 0 < rL (2)");
    }

    /// A countdown loop with no `GREG` directive at all -- keeps
    /// `initialize()`'s default `rG = 32`, the only case the collapse
    /// applies to.
    const NO_GREG_LOOP_MMS: &str =
        "\tLOC\t#100\nMain\tSETL\t$1,5\nLoop\tSUBI\t$1,$1,1\n\tBNZ\t$1,Loop\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn visible_registers_collapse_the_unallocated_global_range() {
        let mmix = assemble(NO_GREG_LOOP_MMS, "loop.mms");
        assert_eq!(
            mmix.get_special(SpecialReg::RG),
            32,
            "no GREG directive: rG stays at initialize()'s default"
        );

        let rows = visible_registers(&mmix);
        let collapsed = rows
            .iter()
            .filter(|row| matches!(row, RegisterRow::UnallocatedGlobalRange))
            .count();
        assert_eq!(
            collapsed, 1,
            "the whole $32..$255 range must collapse into one summary row"
        );

        // Deleting the collapse would instead produce one row per register
        // in $32..$255 -- 224 individually, all zero before any register
        // in that range is ever written.
        let individual_globals = rows
            .iter()
            .filter(|row| matches!(row, RegisterRow::Register { index, .. } if *index >= 32))
            .count();
        assert_eq!(individual_globals, 0);
    }

    #[test]
    fn memory_runs_tag_the_text_label_with_its_full_loaded_bytes() {
        let mut assembler = MMixAssembler::new(crate::HELLO_WORLD_MMS, "hello.mms");
        assembler.parse().expect("hello_world.mms must assemble");
        let mut mmix = MMix::new();
        write_image(&mut mmix, &assembler);

        let text_addr = *assembler
            .labels
            .get("Text")
            .expect("hello_world.mms defines a Text label");
        assert_eq!(text_addr, 0x2000_0000_0000_0000);

        let runs = memory_runs(&mmix, &assembler.labels);
        let run = runs
            .iter()
            .find(|run| {
                run.labels
                    .iter()
                    .any(|(addr, name)| *addr == text_addr && name == "Text")
            })
            .expect("a run must be tagged with the Text label");

        // 14 bytes via loaded_extent(), including the trailing 0 that
        // occupied() would drop.
        let offset = (text_addr - run.start) as usize;
        assert_eq!(&run.bytes[offset..offset + 14], b"Hello world!\n\0");
    }

    #[test]
    fn memory_runs_stay_fixed_across_a_real_run_via_loaded_extent() {
        let mut control =
            crate::control::Control::new(crate::HELLO_WORLD_MMS, "hello.mms").expect("assembles");

        let before = memory_runs(control.machine(), control.labels());
        let before_len: usize = before.iter().map(|run| run.bytes.len()).sum();
        let occupied_before = control.machine().occupied().count();

        let outcome = control.run_chunk(1_000_000);
        assert_eq!(outcome, crate::control::StepOutcome::Halted);
        assert!(control.is_halted(), "the run must actually reach a halt");

        let occupied_after = control.machine().occupied().count();
        assert!(
            occupied_after > occupied_before,
            "occupied() must grow across the run -- guards against a vacuous \
             pass where nothing actually executed"
        );

        let after = memory_runs(control.machine(), control.labels());
        let after_len: usize = after.iter().map(|run| run.bytes.len()).sum();

        // 82 text + 14 data, per HELLO_WORLD_MMS as embedded in main.rs
        // today -- unchanged by the run, since loaded_extent() tracks only
        // what write_image loaded, not the register-stack spills a real
        // run performs.
        assert_eq!(before_len, 96);
        assert_eq!(after_len, before_len);
    }
}
