//! Machine pane: general registers, special registers, loaded memory, and
//! the program's captured output.
//!
//! Computation is plain functions over `&MMix` and the assembler's label
//! table (`AGENTS.md`'s rule that logic not needing browser APIs stays
//! host-testable); [`MachinePane`] only renders their *owned* output --
//! `Properties` must be `'static`, so a borrowed `&MMix` can't cross that
//! boundary. [`RegisterContinuity`]/[`SpecialContinuity`] and the `diff_*`
//! functions are the same kind of plain, testable state: cross-render view
//! state `App` owns, not machine state.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use checksmix::{MMix, SpecialReg};
use web_sys::Element;
use yew::prelude::*;

use crate::control::{OutputSpan, OutputStream};

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
/// glance, narrow enough to fit one line. Every row is aligned to this
/// width, per `docs/layout-spec.md`'s Memory pane section.
const MEMORY_ROW_WIDTH: usize = 16;

/// One row of the visible general-register table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterRow {
    /// Register `$index`, individually visible under the full ISA rule.
    Register { index: u8, value: u64 },
    /// A contiguous, all-zero sub-range of `$32..=$255` (inclusive bounds),
    /// collapsed into one row: no `GREG` directive ran, so this stretch is
    /// genuinely unallocated (never gated on value alone -- `Control` has
    /// no way to tell an allocated-but-zero global from a never-allocated
    /// one, so only registers already known zero ever fold in here). A
    /// nonzero register inside `$32..=$255` still renders individually and
    /// splits the collapse around it.
    UnallocatedGlobalRange { start: u8, end: u8 },
}

/// The 3-clause visibility rule shared by `visible_registers`'s per-index
/// check and `RegisterContinuity::observe`'s sticky union -- factored out so
/// there is exactly one place this rule can diverge (`fb232f8` fixed one
/// such divergence, the `rG == 32` collapse gate, when it lived only in
/// `visible_registers`'s loop).
fn register_included(index: u8, value: u64, rl: u64, rg: u64) -> bool {
    let addr = u64::from(index);
    value != 0 || addr < rl || addr >= rg
}

/// Whether index `index` folds into the no-`GREG` collapse row rather than
/// rendering (or being remembered as sticky) individually: `rG` still holds
/// `initialize()`'s untouched default, `index` sits in the range that
/// default would otherwise mark global via `register_included`'s `i >= rG`
/// clause, and its value is zero. Shared by `visible_registers`'s collapse
/// branch and `RegisterContinuity::observe`, for the same reason
/// `register_included` itself is shared: without this gate, `i >= rG`
/// trivially holds for every index in `$32..=$255` whenever `rG == 32`, so
/// `observe` would mark the entire range sticky on its very first call and
/// permanently defeat the collapse.
fn register_collapses(index: u8, value: u64, rg: u64) -> bool {
    rg == NO_GREG_RG && u64::from(index) >= NO_GREG_RG && value == 0
}

/// A sticky key set, keyed by a small `u8` code -- the shared implementation
/// behind [`RegisterContinuity`] (keyed on register index) and
/// [`SpecialContinuity`] (keyed on `SpecialReg as u8`). A key stays once
/// inserted; there is no removal short of replacing the tracker itself
/// (Reset, or a successful reload).
#[derive(Debug, Clone, Default)]
struct StickySet {
    keys: BTreeSet<u8>,
}

impl StickySet {
    fn insert(&mut self, key: u8) {
        self.keys.insert(key);
    }

    fn contains(&self, key: u8) -> bool {
        self.keys.contains(&key)
    }
}

/// Cross-*render* register-visibility stability: once index `i` satisfies
/// [`register_included`] at any point since the last load/reload, it
/// renders individually from then on. View state, not machine state, per
/// `docs/layout-spec.md`'s Registers section -- `App` owns one instance and
/// replaces it wholesale on Reset/reload.
#[derive(Debug, Clone, Default)]
pub struct RegisterContinuity(StickySet);

impl RegisterContinuity {
    pub fn new() -> Self {
        Self::default()
    }

    /// Union in every currently-visible index under the 3-clause predicate
    /// -- skipping an index the no-`GREG` collapse currently folds away, so
    /// this can never mark the whole collapsed range sticky on one
    /// observation (`register_included`'s `i >= rG` clause trivially holds
    /// for all of `$32..=$255` whenever `rG == 32`).
    pub fn observe(&mut self, mmix: &MMix) {
        let rg = mmix.get_special(SpecialReg::RG);
        let rl = mmix.get_special(SpecialReg::RL);
        for i in 0u16..256 {
            let index = i as u8;
            let value = mmix.get_register(index);
            if register_collapses(index, value, rg) {
                continue;
            }
            if register_included(index, value, rl, rg) {
                self.0.insert(index);
            }
        }
    }

    fn contains(&self, index: u8) -> bool {
        self.0.contains(index)
    }
}

/// Cross-render special-register visibility: any non-pinned special that
/// has ever been nonzero keeps rendering. Specials have no local/global
/// split to key on, so the predicate is plain `value != 0` -- a distinct
/// concrete tracker from [`RegisterContinuity`], sharing [`StickySet`]'s
/// implementation.
#[derive(Debug, Clone, Default)]
pub struct SpecialContinuity(StickySet);

impl SpecialContinuity {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&mut self, mmix: &MMix) {
        for n in 0u8..32 {
            let Some(reg) = SpecialReg::from_u8(n) else {
                continue;
            };
            if mmix.get_special(reg) != 0 {
                self.0.insert(n);
            }
        }
    }

    fn contains(&self, reg: SpecialReg) -> bool {
        self.0.contains(reg as u8)
    }
}

/// Visible general registers: `$0`-`$31` always render (the pinned local-
/// register floor), any register satisfying [`register_included`] renders,
/// and any register that has ever satisfied it since the last load renders
/// too (`continuity`'s sticky set) -- ascending order, a row never moves
/// once shown. When `rG` still holds `initialize()`'s default (no `GREG`
/// directive ran), the all-zero, non-sticky run within `$32..=$255`
/// collapses into one summary row per contiguous stretch.
pub fn visible_registers(mmix: &MMix, continuity: &RegisterContinuity) -> Vec<RegisterRow> {
    let rg = mmix.get_special(SpecialReg::RG);
    let rl = mmix.get_special(SpecialReg::RL);
    let mut rows = Vec::new();
    let mut collapse_start: Option<u8> = None;

    for i in 0u16..256 {
        let index = i as u8;
        let value = mmix.get_register(index);
        let sticky = continuity.contains(index);

        if register_collapses(index, value, rg) && !sticky {
            collapse_start.get_or_insert(index);
            continue;
        }
        if let Some(start) = collapse_start.take() {
            rows.push(RegisterRow::UnallocatedGlobalRange {
                start,
                end: index - 1,
            });
        }
        let pinned = index < 32;
        if pinned || sticky || register_included(index, value, rl, rg) {
            rows.push(RegisterRow::Register { index, value });
        }
    }
    if let Some(start) = collapse_start.take() {
        rows.push(RegisterRow::UnallocatedGlobalRange { start, end: 255 });
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
/// one, plus (per `continuity`) any special that has ever been nonzero
/// since the last load.
pub fn visible_specials(mmix: &MMix, continuity: &SpecialContinuity) -> Vec<SpecialRegisterRow> {
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
        if value != 0 || continuity.contains(reg) {
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

/// One displayed row: either 16 aligned cells of a merged run (a real byte
/// as `Some`, a padding cell as `None`, never `00` for padding), or a thin
/// marker between two runs in different segments.
#[derive(Debug, Clone, PartialEq)]
pub enum MemoryRow {
    Data {
        segment: Segment,
        addr: u64,
        cells: [Option<u8>; MEMORY_ROW_WIDTH],
        labels: Vec<String>,
    },
    SegmentBreak,
}

/// A same-segment stretch of one or more [`MemoryRun`]s, merged when a gap
/// between consecutive runs is smaller than one row -- the alignment unit
/// [`memory_rows`] chunks into display rows.
struct MemoryIsland {
    segment: Segment,
    start: u64,
    end: u64,
    bytes: BTreeMap<u64, u8>,
    labels: Vec<(u64, String)>,
}

/// Merge same-segment runs whose gap is smaller than one row, per
/// `docs/layout-spec.md`'s Memory pane section: row identity is the aligned
/// address, so two runs that would otherwise land on the same aligned row
/// must share one island rather than each claiming it independently.
/// Different-segment runs never merge -- MMIX segments sit far enough
/// apart that a gap that size never occurs between them.
fn merge_islands(runs: &[MemoryRun]) -> Vec<MemoryIsland> {
    let mut islands: Vec<MemoryIsland> = Vec::new();

    for run in runs {
        let run_end = run.start + run.bytes.len() as u64;
        let merges = islands.last().is_some_and(|last| {
            last.segment == run.segment
                && run.start >= last.end
                && run.start - last.end < MEMORY_ROW_WIDTH as u64
        });

        if merges {
            let last = islands.last_mut().expect("just checked non-empty");
            last.end = run_end;
            for (i, &byte) in run.bytes.iter().enumerate() {
                last.bytes.insert(run.start + i as u64, byte);
            }
            last.labels.extend(run.labels.iter().cloned());
        } else {
            let bytes = run
                .bytes
                .iter()
                .enumerate()
                .map(|(i, &byte)| (run.start + i as u64, byte))
                .collect();
            islands.push(MemoryIsland {
                segment: run.segment,
                start: run.start,
                end: run_end,
                bytes,
                labels: run.labels.clone(),
            });
        }
    }

    islands
}

/// Chunk each run into 16-byte-aligned display rows. A run whose start or
/// end doesn't land on the boundary pads with blank (`None`) cells; a
/// segment change between islands gets a [`MemoryRow::SegmentBreak`].
pub fn memory_rows(runs: &[MemoryRun]) -> Vec<MemoryRow> {
    let mut rows = Vec::new();
    let mut last_segment: Option<Segment> = None;

    for island in merge_islands(runs) {
        if last_segment.is_some_and(|segment| segment != island.segment) {
            rows.push(MemoryRow::SegmentBreak);
        }
        last_segment = Some(island.segment);

        let width = MEMORY_ROW_WIDTH as u64;
        let aligned_start = island.start - island.start % width;
        let aligned_end = island.end.div_ceil(width) * width;

        let mut row_start = aligned_start;
        while row_start < aligned_end {
            let mut cells = [None; MEMORY_ROW_WIDTH];
            for (offset, cell) in cells.iter_mut().enumerate() {
                *cell = island.bytes.get(&(row_start + offset as u64)).copied();
            }
            let row_end = row_start + width;
            let labels = island
                .labels
                .iter()
                .filter(|(addr, _)| *addr >= row_start && *addr < row_end)
                .map(|(_, name)| name.clone())
                .collect();
            rows.push(MemoryRow::Data {
                segment: island.segment,
                addr: row_start,
                cells,
                labels,
            });
            row_start += width;
        }
    }

    rows
}

/// Whether `row` is the current-instruction row: `marker_pc` names a real,
/// non-padding byte inside it. Never true for a padding cell or a
/// [`MemoryRow::SegmentBreak`].
pub fn memory_row_is_current(row: &MemoryRow, marker_pc: u64) -> bool {
    match row {
        MemoryRow::Data { addr, cells, .. } => {
            marker_pc >= *addr
                && marker_pc < addr + MEMORY_ROW_WIDTH as u64
                && cells[(marker_pc - addr) as usize].is_some()
        }
        MemoryRow::SegmentBreak => false,
    }
}

/// Cell offsets inside `row` covered by the 4-byte instruction span starting
/// at `marker_pc`. MMIX instructions are tetra-aligned, so a span can never
/// straddle a 16-byte row boundary; empty unless `memory_row_is_current`
/// holds for the same `row`/`marker_pc`.
pub fn memory_row_instruction_span(row: &MemoryRow, marker_pc: u64) -> Vec<usize> {
    let MemoryRow::Data { addr, cells, .. } = row else {
        return Vec::new();
    };
    (0..4u64)
        .filter_map(|i| {
            let byte_addr = marker_pc.checked_add(i)?;
            if byte_addr < *addr || byte_addr >= addr + MEMORY_ROW_WIDTH as u64 {
                return None;
            }
            let offset = (byte_addr - addr) as usize;
            cells[offset].is_some().then_some(offset)
        })
        .collect()
}

/// Every index's known value across `rows`: an individually-rendered
/// register's explicit value, or `0` for every index folded into an
/// unallocated-range collapse (the collapse only ever holds zero registers).
/// An index absent from both source and result was invisible in this
/// snapshot, with no recoverable value.
fn register_value_map(rows: &[RegisterRow]) -> BTreeMap<u8, u64> {
    let mut map = BTreeMap::new();
    for row in rows {
        match row {
            RegisterRow::Register { index, value } => {
                map.insert(*index, *value);
            }
            RegisterRow::UnallocatedGlobalRange { start, end } => {
                for i in *start..=*end {
                    map.insert(i, 0);
                }
            }
        }
    }
    map
}

/// The register indices whose value differs between `prev` and `curr`,
/// where both snapshots have a known value for that index (an index that
/// merely appears or disappears -- e.g. moving in or out of the collapse --
/// is unaffected unless its value actually changed).
pub fn diff_registers(prev: &[RegisterRow], curr: &[RegisterRow]) -> BTreeSet<u8> {
    let prev_map = register_value_map(prev);
    let curr_map = register_value_map(curr);
    curr_map
        .into_iter()
        .filter(|(index, value)| {
            prev_map
                .get(index)
                .is_some_and(|prev_value| prev_value != value)
        })
        .map(|(index, _)| index)
        .collect()
}

fn special_value_map(rows: &[SpecialRegisterRow]) -> BTreeMap<&str, u64> {
    rows.iter()
        .map(|row| (row.name.as_str(), row.value))
        .collect()
}

/// The special-register names whose value differs between `prev` and
/// `curr`, same rule as [`diff_registers`].
pub fn diff_specials(prev: &[SpecialRegisterRow], curr: &[SpecialRegisterRow]) -> BTreeSet<String> {
    let prev_map = special_value_map(prev);
    curr.iter()
        .filter(|row| {
            prev_map
                .get(row.name.as_str())
                .is_some_and(|&prev_value| prev_value != row.value)
        })
        .map(|row| row.name.clone())
        .collect()
}

fn memory_value_map(rows: &[MemoryRow]) -> BTreeMap<u64, u8> {
    let mut map = BTreeMap::new();
    for row in rows {
        if let MemoryRow::Data { addr, cells, .. } = row {
            for (offset, cell) in cells.iter().enumerate() {
                if let Some(byte) = cell {
                    map.insert(addr + offset as u64, *byte);
                }
            }
        }
    }
    map
}

/// The memory addresses whose byte differs between `prev` and `curr`, same
/// rule as [`diff_registers`] -- a padding cell (`None`) is never a known
/// value, so it never contributes a diff entry.
pub fn diff_memory(prev: &[MemoryRow], curr: &[MemoryRow]) -> BTreeSet<u64> {
    let prev_map = memory_value_map(prev);
    let curr_map = memory_value_map(curr);
    curr_map
        .into_iter()
        .filter(|(addr, value)| {
            prev_map
                .get(addr)
                .is_some_and(|prev_value| prev_value != value)
        })
        .map(|(addr, _)| addr)
        .collect()
}

#[derive(Properties, PartialEq)]
pub struct MachinePaneProps {
    pub registers: Vec<RegisterRow>,
    pub specials: Vec<SpecialRegisterRow>,
    pub memory: Vec<MemoryRow>,
    pub pc: u64,
    /// "Where you are": `get_pc()` while running/paused, `get_pc() - 4`
    /// once halted -- see `Control::marker_pc`. Drives the memory pane's
    /// current-row and current-instruction highlights.
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
                <span>{ format!("PC 0x{:016X}", props.pc) }</span>
                <span>{ format!("call depth {}", props.call_depth) }</span>
                { for props.exit_code.map(|code| html! { <span>{ format!("exit {code}") }</span> }) }
            </div>
            <div class="registers-scroll">
                <section class="registers">
                    <h2>{ "Registers" }</h2>
                    <div class="register-grid">
                        { for props.registers.iter().map(|row| render_register_row(row, &props.changed_registers)) }
                    </div>
                </section>
                <section class="specials">
                    <h2>{ "Special registers" }</h2>
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

fn render_register_row(row: &RegisterRow, changed: &BTreeSet<u8>) -> Html {
    match row {
        RegisterRow::Register { index, value } => {
            let is_changed = changed.contains(index);
            let mut hex_class = classes!("reg-hex");
            let mut dec_class = classes!("reg-dec");
            if is_changed {
                hex_class.push("changed");
                dec_class.push("changed");
            }
            html! {
                <div class="register-row">
                    <span class="reg-name">{ format!("${index}") }</span>
                    <span class={hex_class}>{ format!("0x{value:016X}") }</span>
                    <span class={dec_class}>{ (*value as i64).to_string() }</span>
                </div>
            }
        }
        RegisterRow::UnallocatedGlobalRange { start, end } => {
            let count = u32::from(*end) - u32::from(*start) + 1;
            html! {
                <div class="register-row register-collapsed">
                    <span class="reg-name">{ format!("${start}\u{2013}${end}") }</span>
                    <span class="reg-note">{ format!("{count} unallocated (0)") }</span>
                </div>
            }
        }
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
    html! {
        <div class="register-row">
            <span class="reg-name">{ &row.name }</span>
            <span class={hex_class}>{ format!("0x{:016X}", row.value) }</span>
            <span class={dec_class}>{ (row.value as i64).to_string() }</span>
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

#[derive(Properties, PartialEq)]
pub struct OutputPaneProps {
    pub spans: Vec<OutputSpan>,
    /// Mirrored from the status line once halted, per `docs/layout-spec.md`'s
    /// Output pane section, so the result of a run reads in one place.
    pub exit_code: Option<u64>,
}

/// The output pane: the program's captured stdout/stderr/diagnostic output,
/// pinned to the bottom while new output arrives. A user scroll-up unpins
/// it until they scroll back to the bottom themselves -- tracked with a
/// scroll listener rather than re-pinning on every render, which would
/// fight a deliberate scroll-up mid-run.
pub struct OutputPane {
    container_ref: NodeRef,
    pinned: bool,
}

pub enum OutputPaneMsg {
    Scroll,
}

impl Component for OutputPane {
    type Message = OutputPaneMsg;
    type Properties = OutputPaneProps;

    fn create(_ctx: &Context<Self>) -> Self {
        Self {
            container_ref: NodeRef::default(),
            pinned: true,
        }
    }

    fn update(&mut self, _ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            OutputPaneMsg::Scroll => {
                if let Some(el) = self.container_ref.cast::<Element>() {
                    // A couple of pixels of slack: some browsers report a
                    // scroll position that never quite reaches the exact
                    // bottom due to subpixel rounding.
                    let at_bottom = el.scroll_top() + el.client_height() >= el.scroll_height() - 2;
                    self.pinned = at_bottom;
                }
                false
            }
        }
    }

    fn rendered(&mut self, _ctx: &Context<Self>, _first_render: bool) {
        if self.pinned
            && let Some(el) = self.container_ref.cast::<Element>()
        {
            el.set_scroll_top(el.scroll_height());
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let onscroll = ctx.link().callback(|_: Event| OutputPaneMsg::Scroll);
        let header = match ctx.props().exit_code {
            Some(code) => format!("OUTPUT  exit {code}"),
            None => "OUTPUT".to_string(),
        };
        html! {
            <div class="output-pane">
                <div class="output-header">{ header }</div>
                <div class="output-body" ref={self.container_ref.clone()} {onscroll}>
                    { for ctx.props().spans.iter().map(render_output_span) }
                </div>
            </div>
        }
    }
}

fn render_output_span(span: &OutputSpan) -> Html {
    let class = match span.stream {
        OutputStream::Stdout => "output-stdout",
        OutputStream::Stderr => "output-stderr",
        OutputStream::Diagnostic => "output-diagnostic",
    };
    html! { <span {class}>{ &span.text }</span> }
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

        let continuity = RegisterContinuity::new();
        let indices: Vec<u8> = visible_registers(&mmix, &continuity)
            .into_iter()
            .map(|row| match row {
                RegisterRow::Register { index, .. } => index,
                RegisterRow::UnallocatedGlobalRange { .. } => {
                    panic!("rG = 253, not 32; must not collapse")
                }
            })
            .collect();

        // The pinned floor ($0-$31) always renders, plus the three globals
        // via the `i >= rG` clause. Deleting the pinned floor would drop
        // $0-$31; deleting the `i >= rG` clause would drop $253-$255.
        let expected: Vec<u8> = (0..=31).chain([253, 254, 255]).collect();
        assert_eq!(indices, expected);
    }

    /// A `GREG` (raising `rG` above the no-`GREG` collapse floor) plus a
    /// local write past index 32, so `rL` grows past 32 too -- isolating
    /// the `i < rL` clause from both the pinned floor ($0-$31) and the
    /// collapse (which only ever fires when `rG == 32`).
    const GREG_AND_LOCAL_MMS: &str =
        "\tLOC\t#100\nG1\tGREG\t@\nMain\tSETL\t$40,7\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn visible_registers_include_untouched_locals_via_i_lt_rl() {
        let mut mmix = assemble(GREG_AND_LOCAL_MMS, "local.mms");
        assert!(mmix.execute_instruction(), "SETL must execute, not halt");

        let rg = mmix.get_special(SpecialReg::RG);
        let rl = mmix.get_special(SpecialReg::RL);
        assert!(
            rg > 32,
            "fixture must allocate a GREG so rG rises above the collapse gate"
        );
        assert!(
            rl > 35 && rl < rg,
            "fixture must grow rL strictly between 35 and rG: rl={rl} rg={rg}"
        );

        let continuity = RegisterContinuity::new();
        // $35 is zero-valued, 32 <= 35 < rL -- the pinned floor doesn't
        // cover it ($0-$31) and the collapse can't reach it (rG != 32).
        let has_35 = visible_registers(&mmix, &continuity).iter().any(|row| {
            matches!(
                row,
                RegisterRow::Register {
                    index: 35,
                    value: 0
                }
            )
        });
        assert!(has_35, "$35 must be visible via the i < rL clause");
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

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(&mmix, &continuity);
        let collapsed: Vec<&RegisterRow> = rows
            .iter()
            .filter(|row| matches!(row, RegisterRow::UnallocatedGlobalRange { .. }))
            .collect();
        assert_eq!(
            collapsed,
            vec![&RegisterRow::UnallocatedGlobalRange {
                start: 32,
                end: 255
            }],
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

    /// Same fixture as `control.rs`'s `CALL_MMS`: no `GREG` at all, but
    /// `SET $255,$0` writes a nonzero value into a register above the
    /// no-GREG collapse floor before `TRAP 0,Halt,0`.
    const CALL_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,40\n\tSETL\t$2,2\n\tPUSHJ\t$0,AddFunc\n\tSET\t$255,$0\n\tTRAP\t0,Halt,0\nAddFunc\tADDU\t$0,$0,$1\n\tPOP\t1,0\n";

    #[test]
    fn visible_registers_show_a_nonzero_global_written_with_no_greg() {
        let mut control = crate::control::Control::new(CALL_MMS, "call.mms").expect("assembles");
        let outcome = control.run_chunk(1_000_000);
        assert_eq!(outcome, crate::control::StepOutcome::Halted);
        assert!(control.is_halted(), "the run must actually reach a halt");
        assert_eq!(
            control.machine().get_special(SpecialReg::RG),
            32,
            "no GREG directive: rG stays at initialize()'s default"
        );

        let value255 = control.machine().get_register(255);
        assert_ne!(value255, 0, "fixture must write a nonzero value into $255");

        // Deleting the fix would collapse $255 into the unallocated-range
        // summary row, hiding its real value behind a false "(0)" label.
        let continuity = RegisterContinuity::new();
        let rows = visible_registers(control.machine(), &continuity);
        let has_individual_255 = rows.iter().any(
            |row| matches!(row, RegisterRow::Register { index: 255, value } if *value == value255),
        );
        assert!(
            has_individual_255,
            "$255's nonzero value must render individually, not be \
             swallowed into the unallocated-range collapse"
        );
    }

    #[test]
    fn register_continuity_keeps_a_once_visible_register_after_it_reverts() {
        let mut control = crate::control::Control::new(CALL_MMS, "call.mms").expect("assembles");
        let mut continuity = RegisterContinuity::new();
        continuity.observe(control.machine());

        let outcome = control.run_chunk(1_000_000);
        assert_eq!(outcome, crate::control::StepOutcome::Halted);
        continuity.observe(control.machine());
        assert_ne!(
            control.machine().get_register(255),
            0,
            "fixture must write a nonzero value into $255"
        );

        // Reload back to a fresh (all-zero-again) machine, keeping the same
        // continuity tracker: $255 must stay visible, sticky from the
        // earlier observation, even though its value is 0 again.
        control.reload(CALL_MMS).expect("still assembles");
        assert_eq!(
            control.machine().get_register(255),
            0,
            "fresh load starts at 0 again"
        );

        let rows = visible_registers(control.machine(), &continuity);
        assert!(
            rows.iter().any(|row| matches!(
                row,
                RegisterRow::Register {
                    index: 255,
                    value: 0
                }
            )),
            "$255 must stay visible under the sticky rule"
        );

        // A truly untouched, non-pinned register from the same run must
        // still be absent.
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row, RegisterRow::Register { index: 100, .. })),
            "an untouched, never-visible register must stay hidden"
        );

        // A fresh RegisterContinuity, as Reset/SourceChanged would
        // construct on a successful reload, starts with an empty sticky
        // set.
        let fresh = RegisterContinuity::new();
        let fresh_rows = visible_registers(control.machine(), &fresh);
        assert!(
            !fresh_rows
                .iter()
                .any(|row| matches!(row, RegisterRow::Register { index: 255, .. })),
            "a fresh RegisterContinuity must start with an empty sticky set"
        );
    }

    #[test]
    fn special_continuity_keeps_a_once_nonzero_special_after_it_reverts() {
        let mut mmix = MMix::new();
        assert!(
            !PINNED_SPECIALS.contains(&SpecialReg::RQ),
            "fixture must use a non-pinned special"
        );

        let mut continuity = SpecialContinuity::new();
        continuity.observe(&mmix);

        mmix.set_special(SpecialReg::RQ, 7);
        continuity.observe(&mmix);
        mmix.set_special(SpecialReg::RQ, 0);

        let rows = visible_specials(&mmix, &continuity);
        assert!(
            rows.iter().any(|row| row.name == "rQ" && row.value == 0),
            "rQ must stay visible under the sticky rule"
        );
        assert!(
            !rows.iter().any(|row| row.name == "rU"),
            "an untouched special must stay hidden"
        );

        let fresh = SpecialContinuity::new();
        let fresh_rows = visible_specials(&mmix, &fresh);
        assert!(
            !fresh_rows.iter().any(|row| row.name == "rQ"),
            "a fresh SpecialContinuity must start with an empty sticky set"
        );
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

    #[test]
    fn memory_rows_pad_a_misaligned_run_start() {
        let run = MemoryRun {
            segment: Segment::Text,
            start: 0x104,
            bytes: vec![0xAA; 4],
            labels: Vec::new(),
        };
        let rows = memory_rows(&[run]);
        let MemoryRow::Data { addr, cells, .. } = &rows[0] else {
            panic!("expected a data row");
        };
        assert_eq!(
            *addr, 0x100,
            "row address must align down to the 16-byte boundary"
        );
        for cell in &cells[0..4] {
            assert_eq!(*cell, None, "leading padding cell must be blank, not 0x00");
        }
        for cell in &cells[4..8] {
            assert_eq!(*cell, Some(0xAA));
        }
    }

    #[test]
    fn memory_rows_pad_a_short_trailing_row() {
        let run = MemoryRun {
            segment: Segment::Text,
            start: 0x100,
            // Spans two rows: 0x100..0x110 full, 0x110..0x114 partial.
            bytes: vec![0xBB; 20],
            labels: Vec::new(),
        };
        let rows = memory_rows(&[run]);
        assert_eq!(rows.len(), 2);
        let MemoryRow::Data { addr, cells, .. } = &rows[1] else {
            panic!("expected a data row");
        };
        assert_eq!(*addr, 0x110);
        for cell in &cells[0..4] {
            assert_eq!(*cell, Some(0xBB));
        }
        for cell in &cells[4..16] {
            assert_eq!(*cell, None, "trailing padding cell must be blank, not 0x00");
        }
    }

    #[test]
    fn memory_rows_merge_close_runs_in_the_same_segment() {
        let run_a = MemoryRun {
            segment: Segment::Data,
            start: 0x2000_0000_0000_0000,
            bytes: vec![1, 2, 3],
            labels: Vec::new(),
        };
        // A 5-byte gap (bytes 3..8), well under one 16-byte row.
        let run_b = MemoryRun {
            segment: Segment::Data,
            start: 0x2000_0000_0000_0008,
            bytes: vec![9, 9],
            labels: Vec::new(),
        };
        let rows = memory_rows(&[run_a, run_b]);
        assert_eq!(rows.len(), 1, "both runs fit in one merged 16-byte row");

        let MemoryRow::Data { addr, cells, .. } = &rows[0] else {
            panic!("expected a data row");
        };
        let addrs: BTreeSet<u64> = [*addr].into_iter().collect();
        assert_eq!(
            addrs.len(),
            1,
            "merged runs must not produce colliding row addresses"
        );
        assert_eq!(cells[0], Some(1));
        assert_eq!(
            cells[3], None,
            "the gap between runs must render as padding"
        );
        assert_eq!(cells[8], Some(9));
    }

    #[test]
    fn memory_row_flags_the_current_instruction_and_its_span() {
        let run = MemoryRun {
            segment: Segment::Text,
            start: 0x100,
            bytes: vec![0; 16],
            labels: Vec::new(),
        };
        let rows = memory_rows(&[run]);
        let row = &rows[0];

        let marker_pc = 0x108;
        assert!(memory_row_is_current(row, marker_pc));
        assert_eq!(
            memory_row_instruction_span(row, marker_pc),
            vec![8, 9, 10, 11]
        );

        let other_run = MemoryRun {
            segment: Segment::Text,
            start: 0x200,
            bytes: vec![0; 16],
            labels: Vec::new(),
        };
        let other_rows = memory_rows(&[other_run]);
        assert!(!memory_row_is_current(&other_rows[0], marker_pc));
        assert!(memory_row_instruction_span(&other_rows[0], marker_pc).is_empty());
    }

    #[test]
    fn memory_row_marker_uses_the_halted_adjustment() {
        // Mirrors Control::marker_pc: while halted, the real last
        // instruction sat 4 bytes before get_pc(). The memory pane must
        // flag that instruction, not the (past-the-end) raw PC.
        let run = MemoryRun {
            segment: Segment::Text,
            start: 0x100,
            bytes: vec![0; 16],
            labels: Vec::new(),
        };
        let rows = memory_rows(&[run]);
        let row = &rows[0];

        let raw_pc_after_halt = 0x110; // past this run entirely
        let halted_marker_pc = raw_pc_after_halt - 4; // the real last instruction

        assert!(
            !memory_row_is_current(row, raw_pc_after_halt),
            "the raw halted PC must not flag any row here"
        );
        assert!(memory_row_is_current(row, halted_marker_pc));
        assert_eq!(
            memory_row_instruction_span(row, halted_marker_pc),
            vec![12, 13, 14, 15]
        );
    }

    #[test]
    fn diff_registers_flags_only_indices_whose_value_differs() {
        let prev = vec![
            RegisterRow::Register { index: 1, value: 5 },
            RegisterRow::Register { index: 2, value: 9 },
            RegisterRow::UnallocatedGlobalRange {
                start: 32,
                end: 255,
            },
        ];
        let curr = vec![
            RegisterRow::Register { index: 1, value: 5 }, // unchanged
            RegisterRow::Register {
                index: 2,
                value: 10,
            }, // changed
            // Newly individually visible (moved out of the collapse), but
            // still zero -- must not be flagged.
            RegisterRow::Register {
                index: 40,
                value: 0,
            },
        ];

        let diff = diff_registers(&prev, &curr);
        assert_eq!(diff, BTreeSet::from([2]));
    }

    #[test]
    fn diff_memory_flags_only_addresses_whose_byte_differs() {
        let mut prev_cells = [None; MEMORY_ROW_WIDTH];
        prev_cells[0] = Some(1);
        prev_cells[1] = Some(2);
        let prev = vec![MemoryRow::Data {
            segment: Segment::Text,
            addr: 0x100,
            cells: prev_cells,
            labels: Vec::new(),
        }];

        let mut curr_cells = [None; MEMORY_ROW_WIDTH];
        curr_cells[0] = Some(1); // unchanged
        curr_cells[1] = Some(9); // changed
        let curr = vec![MemoryRow::Data {
            segment: Segment::Text,
            addr: 0x100,
            cells: curr_cells,
            labels: Vec::new(),
        }];

        let diff = diff_memory(&prev, &curr);
        assert_eq!(diff, BTreeSet::from([0x101]));
    }
}
