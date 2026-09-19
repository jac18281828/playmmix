//! General and special registers: classification, continuity, and the
//! visible register/special rows.

use std::collections::BTreeSet;

use checksmix::{MMix, SpecialReg};

/// `rG`'s value with no `GREG` directive at all: `MMix::initialize`'s
/// default. `write_image` only ever raises `rG` above this floor, and only
/// when a program uses `GREG` -- but neither direction of that implication
/// is exact, so this value never stands alone as "nothing was allocated".
/// 223 `GREG` directives allocate downward from `$254` to exactly `$32` and
/// leave `rG` here for a real allocation, and `PUT`/`PUTI` write `rG`
/// unvalidated, moving it off this default with no `GREG` at all. See
/// [`register_collapses`], which pairs this check with
/// `Control::has_greg_allocations`.
const NO_GREG_RG: u64 = 32;

/// The six special registers always shown, regardless of value -- how the
/// register stack is taught.
pub(super) const PINNED_SPECIALS: [SpecialReg; 6] = [
    SpecialReg::RA,
    SpecialReg::RG,
    SpecialReg::RL,
    SpecialReg::RO,
    SpecialReg::RS,
    SpecialReg::RJ,
];

/// MMIX's three-way split of the 256 general registers, by `rL` and `rG`:
/// local (`$0`..`$(rL-1)`, the current call frame), marginal (`$rL`..
/// `$(rG-1)`, reads as zero and raises `rL` on write), global (`$rG`..
/// `$255`). Never derived from a register's index alone -- `GREG` can raise
/// `rG` above 32, which puts marginal registers above `$31` too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterClass {
    Local,
    Marginal,
    Global,
}

/// Classify `index` under the current `rL`/`rG`, per [`RegisterClass`]'s
/// three ranges.
fn register_class(index: u8, rl: u64, rg: u64) -> RegisterClass {
    let addr = u64::from(index);
    if addr < rl {
        RegisterClass::Local
    } else if addr < rg {
        RegisterClass::Marginal
    } else {
        RegisterClass::Global
    }
}

/// One row of the visible general-register table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterRow {
    /// Register `$index`, individually visible under the full ISA rule.
    /// `mark` names `rL`'s value on the one row where the local/marginal
    /// boundary is shown; `None` on every other row.
    Register {
        index: u8,
        value: u64,
        class: RegisterClass,
        mark: Option<u64>,
    },
    /// A contiguous run of zero-valued global registers within `$32..=$254`
    /// (inclusive bounds), collapsed into one row: no `GREG` directive ran
    /// *and* `rG` still holds its untouched default, so every register in
    /// the run is global (never gated on value alone -- an allocated-but-
    /// zero global is indistinguishable from a never-allocated one by
    /// value, so only registers already known zero ever fold in here). A
    /// nonzero register inside `$32..=$254` still renders individually and
    /// splits the run around it. `$255` never folds in here; see
    /// `register_collapses`.
    ZeroGlobalRange { start: u8, end: u8 },
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

/// Whether index `index` folds into the collapsed global-range row rather
/// than rendering (or being remembered as sticky) individually: no `GREG`
/// ran (`has_greg`, from `Control::has_greg_allocations`), `rG` still holds
/// `initialize()`'s untouched default, `index` sits in the range that
/// default would otherwise mark global via `register_included`'s `i >= rG`
/// clause, its value is zero, and `index` is not `$255`.
///
/// The `!has_greg`/`rg == NO_GREG_RG` pair is required, because each alone
/// admits a case the other rules out. `rG == 32` also holds after 223 real
/// `GREG` directives, which allocate downward from `$254` to exactly `$32`
/// -- folding a genuinely allocated range into the collapse. `!has_greg`
/// also holds after a `PUT`/`PUTI` moves `rG` with no `GREG` anywhere in the
/// program -- folding away a range `register_included`'s `i >= rG` clause
/// has already decided is global. `index != 255` keeps `$255` out of the
/// collapse unconditionally, so it always renders individually.
///
/// Shared by `visible_registers`'s collapse branch and
/// `RegisterContinuity::observe`, for the same reason `register_included`
/// itself is shared: without the `rG`/`has_greg` gate, `i >= rG` trivially
/// holds for every index in `$32..=$254` whenever `rG == 32`, so `observe`
/// would mark the entire range sticky on its very first call and
/// permanently defeat the collapse.
fn register_collapses(index: u8, value: u64, rg: u64, has_greg: bool) -> bool {
    !has_greg && rg == NO_GREG_RG && u64::from(index) >= NO_GREG_RG && index != 255 && value == 0
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
    /// -- skipping an index the global-range collapse currently folds away,
    /// so this can never mark the whole collapsed range sticky on one
    /// observation (`register_included`'s `i >= rG` clause trivially holds
    /// for all of `$32..=$254` whenever `rG == 32`). `has_greg` comes from
    /// `Control::has_greg_allocations`, and must match what the caller
    /// passes [`visible_registers`]: the two share [`register_collapses`]
    /// precisely so they cannot disagree about what folds away.
    pub fn observe(&mut self, mmix: &MMix, has_greg: bool) {
        let rg = mmix.get_special(SpecialReg::RG);
        let rl = mmix.get_special(SpecialReg::RL);
        for i in 0u16..256 {
            let index = i as u8;
            let value = mmix.get_register(index);
            if register_collapses(index, value, rg, has_greg) {
                continue;
            }
            if register_included(index, value, rl, rg) {
                self.0.insert(index);
            }
        }
    }

    pub(super) fn contains(&self, index: u8) -> bool {
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

/// Visible general registers: `$0`-`$31` always render -- MMIX requires
/// `rG >= 32`, so none of them is ever global -- any register satisfying
/// [`register_included`] renders, and any register that has ever satisfied
/// it since the last load renders too (`continuity`'s sticky set) --
/// ascending order, a row never moves
/// once shown. When no `GREG` ran (`has_greg`, from
/// `Control::has_greg_allocations`) *and* `rG` still holds `initialize()`'s
/// default, the all-zero, non-sticky run within `$32..=$254` collapses into
/// one summary row per contiguous stretch; `$255` never folds in, whatever
/// its value; see [`register_collapses`] for why neither signal suffices
/// alone. Each individually-rendered row carries its [`RegisterClass`] under
/// the same `rL`/`rG`. The first rendered marginal row (ascending index)
/// carries the `rL` mark, naming `rL`'s value; when `rL` is 0 that row is
/// `$0`, and a load with no marginal row rendered carries no mark at all.
pub fn visible_registers(
    mmix: &MMix,
    continuity: &RegisterContinuity,
    has_greg: bool,
) -> Vec<RegisterRow> {
    let rg = mmix.get_special(SpecialReg::RG);
    let rl = mmix.get_special(SpecialReg::RL);
    let mut rows = Vec::new();
    let mut collapse_start: Option<u8> = None;

    for i in 0u16..256 {
        let index = i as u8;
        let value = mmix.get_register(index);
        let sticky = continuity.contains(index);

        if register_collapses(index, value, rg, has_greg) && !sticky {
            collapse_start.get_or_insert(index);
            continue;
        }
        if let Some(start) = collapse_start.take() {
            rows.push(RegisterRow::ZeroGlobalRange {
                start,
                end: index - 1,
            });
        }
        let pinned = index < 32;
        if pinned || sticky || register_included(index, value, rl, rg) {
            rows.push(RegisterRow::Register {
                index,
                value,
                class: register_class(index, rl, rg),
                mark: None,
            });
        }
    }
    if let Some(start) = collapse_start.take() {
        rows.push(RegisterRow::ZeroGlobalRange { start, end: 254 });
    }

    mark_rl_boundary(&mut rows, rl);

    rows
}

/// Assign the `rL` mark to the first rendered marginal row (ascending
/// index), naming `rL`'s value -- not necessarily the row whose index
/// equals `rL`, since that row may not itself be rendered. A no-op when no
/// rendered row is marginal.
fn mark_rl_boundary(rows: &mut [RegisterRow], rl: u64) {
    let mark = rows.iter_mut().find_map(|row| match row {
        RegisterRow::Register {
            class: RegisterClass::Marginal,
            mark,
            ..
        } => Some(mark),
        _ => None,
    });
    if let Some(mark) = mark {
        *mark = Some(rl);
    }
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
pub(super) fn special_reg_name(reg: SpecialReg) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use checksmix::{MMixAssembler, entry_point, start_program, write_image};

    use crate::machine::fixtures::{CALL_MMS, REVERTING_GLOBAL_MMS, TWO_GREG_MMS};
    use crate::machine::pane::collapsed_range_note;

    /// Assemble `source` and load it, unexecuted -- the same shape
    /// `Control::assemble_and_load` uses, restated here so these tests
    /// don't need a `Control`. Returns the collapse's `has_greg` signal
    /// alongside, read from the same `greg_inits` list
    /// `Control::has_greg_allocations` reads, since the assembler itself
    /// doesn't outlive this call.
    fn assemble(source: &str, filename: &str) -> (MMix, bool) {
        let mut assembler = MMixAssembler::new(source, filename);
        assembler.parse().expect("test program assembles");
        let mut mmix = MMix::new();
        write_image(&mut mmix, &assembler);
        start_program(&mut mmix, entry_point(&assembler));
        (mmix, !assembler.greg_inits.is_empty())
    }

    #[test]
    fn visible_registers_include_allocated_zero_globals_via_i_ge_rg() {
        let (mmix, has_greg) = assemble(TWO_GREG_MMS, "two_greg.mms");
        assert_eq!(
            mmix.get_special(SpecialReg::RG),
            253,
            "fixture must allocate two GREGs starting at $253"
        );
        assert_eq!(mmix.get_special(SpecialReg::RL), 0);

        let continuity = RegisterContinuity::new();
        let indices: Vec<u8> = visible_registers(&mmix, &continuity, has_greg)
            .into_iter()
            .map(|row| match row {
                RegisterRow::Register { index, .. } => index,
                RegisterRow::ZeroGlobalRange { .. } => {
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
        let (mut mmix, has_greg) = assemble(GREG_AND_LOCAL_MMS, "local.mms");
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
        let has_35 = visible_registers(&mmix, &continuity, has_greg)
            .iter()
            .any(|row| {
                matches!(
                    row,
                    RegisterRow::Register {
                        index: 35,
                        value: 0,
                        ..
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
    fn visible_registers_collapse_the_zero_valued_global_range() {
        let (mmix, has_greg) = assemble(NO_GREG_LOOP_MMS, "loop.mms");
        assert!(!has_greg, "no GREG directive: greg_inits must be empty");
        assert_eq!(
            mmix.get_special(SpecialReg::RG),
            32,
            "no GREG directive: rG stays at initialize()'s default"
        );

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(&mmix, &continuity, has_greg);
        let collapsed: Vec<&RegisterRow> = rows
            .iter()
            .filter(|row| matches!(row, RegisterRow::ZeroGlobalRange { .. }))
            .collect();
        assert_eq!(
            collapsed,
            vec![&RegisterRow::ZeroGlobalRange {
                start: 32,
                end: 254
            }],
            "the whole $32..$254 range must collapse into one summary row"
        );

        // Deleting the collapse would instead produce one row per register
        // in $32..$254 -- 223 individually, all zero before any register
        // in that range is ever written. $255 always renders individually,
        // whatever the collapse does, so it alone survives this count.
        let individual_globals = rows
            .iter()
            .filter(|row| matches!(row, RegisterRow::Register { index, .. } if *index >= 32))
            .count();
        assert_eq!(individual_globals, 1);
    }

    /// `count` `GREG` directives, each initialized to zero, then an entry
    /// point. `GREG` allocates downward from `$254`, so the count picks the
    /// lowest register allocated -- built here rather than hand-typed
    /// because the count that matters (223, landing exactly on `$32`) is
    /// far too long to read as a literal.
    fn many_gregs(count: usize) -> String {
        let mut source = String::from("\tLOC\t#100\n");
        for i in 0..count {
            source.push_str(&format!("G{i}\tGREG\t0\n"));
        }
        source.push_str("Main\tTRAP\t0,Halt,0\n");
        source
    }

    #[test]
    fn allocated_zero_globals_never_collapse_when_greg_lands_exactly_on_32() {
        // 223 GREGs allocate $254 down to $32, so rG matches
        // initialize()'s untouched default for a real allocation -- the
        // one case where rG alone cannot tell allocated from unallocated.
        let control =
            crate::control::Control::new(&many_gregs(223), "many_greg.mms").expect("assembles");
        assert_eq!(
            control.machine().get_special(SpecialReg::RG),
            NO_GREG_RG,
            "223 GREGs must drive rG down to exactly the no-GREG default"
        );
        assert!(
            control.has_greg_allocations(),
            "the fixture's GREGs must register as a real allocation"
        );

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(
            control.machine(),
            &continuity,
            control.has_greg_allocations(),
        );

        // Dropping the `!has_greg` conjunct folds all 223 of these
        // genuinely allocated, zero-valued globals into the collapse row.
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row, RegisterRow::ZeroGlobalRange { .. })),
            "GREG-allocated registers must never collapse"
        );
        let individual: Vec<u8> = rows
            .iter()
            .filter_map(|row| match row {
                RegisterRow::Register { index, .. } => Some(*index),
                RegisterRow::ZeroGlobalRange { .. } => None,
            })
            .collect();
        assert_eq!(
            individual,
            (0u16..256).map(|i| i as u8).collect::<Vec<u8>>(),
            "every register must render individually: $0-$31 pinned, \
             $32-$255 global via the i >= rG clause"
        );
    }

    /// No `GREG` anywhere, but `PUTI rG,100` moves `rG` at runtime --
    /// `set_special` is a bare store with no validation, so `has_greg` and
    /// `rG` disagree in the opposite direction from `many_gregs(223)`.
    const PUT_RG_MMS: &str = "\tLOC\t#100\nMain\tPUTI\trG,100\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn a_runtime_moved_rg_never_collapses_the_range_it_marks_global() {
        let mut control =
            crate::control::Control::new(PUT_RG_MMS, "put_rg.mms").expect("assembles");
        assert_eq!(
            control.run_chunk(1_000),
            crate::control::StepOutcome::Halted
        );
        assert!(
            !control.has_greg_allocations(),
            "the fixture must declare no GREG at all"
        );
        assert_eq!(
            control.machine().get_special(SpecialReg::RG),
            100,
            "PUTI must move rG off its default with no GREG involved"
        );

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(
            control.machine(),
            &continuity,
            control.has_greg_allocations(),
        );

        // Replacing the `rG == 32` check with `!has_greg` (rather than
        // conjoining them) folds $32-$254 into one collapse row here.
        // Under the conjoined check there is no collapse row at all:
        // $100-$255 render individually via `i >= rG`, and $32-$99 render
        // nothing -- the same empty middle range any rG > 32 produces,
        // pinned by `visible_registers_include_allocated_zero_globals_via_
        // i_ge_rg`.
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row, RegisterRow::ZeroGlobalRange { .. })),
            "a runtime-moved rG must produce no collapse row"
        );
        let individual: Vec<u8> = rows
            .iter()
            .filter_map(|row| match row {
                RegisterRow::Register { index, .. } => Some(*index),
                RegisterRow::ZeroGlobalRange { .. } => None,
            })
            .collect();
        assert_eq!(
            individual,
            (0u16..32)
                .chain(100..256)
                .map(|i| i as u8)
                .collect::<Vec<u8>>()
        );
    }

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

        // Deleting the fix would collapse $255 into the global-range
        // summary row, hiding its real value behind a false "(0)" label.
        let continuity = RegisterContinuity::new();
        let rows = visible_registers(
            control.machine(),
            &continuity,
            control.has_greg_allocations(),
        );
        let has_individual_255 = rows.iter().any(|row| {
            matches!(row, RegisterRow::Register { index: 255, value, .. } if *value == value255)
        });
        assert!(
            has_individual_255,
            "$255's nonzero value must render individually, not be \
             swallowed into the global-range collapse"
        );
    }

    #[test]
    fn register_continuity_keeps_a_once_visible_register_after_it_reverts() {
        // $255 always renders individually, never folding into the
        // collapse, so it can no longer witness stickiness. $40 sits in
        // the collapse range and goes nonzero then back to zero within
        // three instructions, so only per-step observation (not
        // `run_chunk`'s once-per-chunk sampling) ever catches it -- see
        // `sticky_continuity_samples_per_chunk_under_run_and_per_instruction_under_step`.
        let mut control =
            crate::control::Control::new(REVERTING_GLOBAL_MMS, "revert.mms").expect("assembles");
        let has_greg = control.has_greg_allocations();
        let mut continuity = RegisterContinuity::new();
        continuity.observe(control.machine(), has_greg);

        let mut saw_nonzero_40 = false;
        while !control.is_halted() {
            control.step();
            saw_nonzero_40 |= control.machine().get_register(40) != 0;
            continuity.observe(control.machine(), has_greg);
        }
        assert!(
            saw_nonzero_40,
            "fixture must write a nonzero value into $40"
        );

        // Reload back to a fresh (all-zero-again) machine, keeping the same
        // continuity tracker: $40 must stay visible, sticky from the
        // earlier observation, even though its value is 0 again.
        control
            .reload(REVERTING_GLOBAL_MMS)
            .expect("still assembles");
        assert_eq!(
            control.machine().get_register(40),
            0,
            "fresh load starts at 0 again"
        );

        let rows = visible_registers(control.machine(), &continuity, has_greg);
        assert!(
            rows.iter().any(|row| matches!(
                row,
                RegisterRow::Register {
                    index: 40,
                    value: 0,
                    ..
                }
            )),
            "$40 must stay visible under the sticky rule"
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
        let fresh_rows = visible_registers(control.machine(), &fresh, has_greg);
        assert!(
            !fresh_rows
                .iter()
                .any(|row| matches!(row, RegisterRow::Register { index: 40, .. })),
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

    /// No `GREG` at all -- the load-time state: `rL = 0`, `rG = 32`.
    const NO_GREG_HALT_MMS: &str = "\tLOC\t#100\nMain\tTRAP\t0,Halt,0\n";

    #[test]
    fn visible_registers_classify_a_fresh_load_as_marginal_and_global() {
        let (mmix, has_greg) = assemble(NO_GREG_HALT_MMS, "halt.mms");
        assert_eq!(mmix.get_special(SpecialReg::RL), 0);
        assert_eq!(mmix.get_special(SpecialReg::RG), 32);

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(&mmix, &continuity, has_greg);

        // $0-$31: rL = 0, so none of them is local -- every one is marginal.
        for index in 0u8..32 {
            let class = rows.iter().find_map(|row| match row {
                RegisterRow::Register {
                    index: i, class, ..
                } if *i == index => Some(*class),
                _ => None,
            });
            assert_eq!(
                class,
                Some(RegisterClass::Marginal),
                "${index} must be marginal at load"
            );
        }

        let collapse = rows
            .iter()
            .find_map(|row| match row {
                RegisterRow::ZeroGlobalRange { start, end } => Some((*start, *end)),
                _ => None,
            })
            .expect("the untouched $32-$254 range must collapse");
        assert_eq!(collapse, (32, 254));
        assert_eq!(collapsed_range_note(223), "223 global (0)");

        // `start_program`'s start state: $255 holds the entry address, not
        // zero -- an independent oracle, not a readback through `Control`,
        // so this catches the helper drifting from `Control`'s own load
        // path.
        let mut oracle = MMixAssembler::new(NO_GREG_HALT_MMS, "halt.mms");
        oracle.parse().expect("test program assembles");
        let entry = entry_point(&oracle);

        let reg255 = rows
            .iter()
            .find(|row| matches!(row, RegisterRow::Register { index: 255, .. }))
            .expect("$255 must always render, never fold into the collapse");
        assert!(matches!(
            reg255,
            RegisterRow::Register {
                value,
                class: RegisterClass::Global,
                ..
            } if *value == entry
        ));
    }

    #[test]
    fn a_zeroed_dollar_255_still_renders_individually_as_global() {
        // `ViewState::reset` observes the load while $255 holds the entry
        // address (`start_program`), which would make $255 sticky and hide
        // the `register_collapses` exclusion this pins -- render through a
        // fresh continuity instead, not through `ViewState`.
        const ZERO_DOLLAR_255_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$255,0\n\tTRAP\t0,Halt,0\n";
        let mut control =
            crate::control::Control::new(ZERO_DOLLAR_255_MMS, "zero255.mms").expect("assembles");
        assert_eq!(
            control.step(),
            crate::control::StepOutcome::Advanced,
            "SETL must not halt"
        );
        assert_eq!(control.machine().get_register(255), 0, "fixture assumption");

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(
            control.machine(),
            &continuity,
            control.has_greg_allocations(),
        );
        let reg255 = rows
            .iter()
            .find(|row| matches!(row, RegisterRow::Register { index: 255, .. }))
            .expect("$255 must render individually even at zero, never fold into the collapse");
        assert!(matches!(
            reg255,
            RegisterRow::Register {
                value: 0,
                class: RegisterClass::Global,
                ..
            }
        ));
    }

    #[test]
    fn visible_registers_classify_call_mms_after_return_as_local_then_marginal() {
        let mut control = crate::control::Control::new(CALL_MMS, "call.mms").expect("assembles");
        assert_eq!(
            control.run_chunk(1_000_000),
            crate::control::StepOutcome::Halted
        );
        assert_eq!(control.machine().get_special(SpecialReg::RL), 1);

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(
            control.machine(),
            &continuity,
            control.has_greg_allocations(),
        );

        let class_of = |index: u8| {
            rows.iter().find_map(|row| match row {
                RegisterRow::Register {
                    index: i, class, ..
                } if *i == index => Some(*class),
                _ => None,
            })
        };
        assert_eq!(
            class_of(0),
            Some(RegisterClass::Local),
            "rL = 1: $0 is the only local register"
        );
        for index in 1u8..32 {
            assert_eq!(
                class_of(index),
                Some(RegisterClass::Marginal),
                "${index} must be marginal once rL falls back to 1"
            );
        }
    }

    /// A single marginal write, targeting `$5` while `rL` is still 0.
    const MARGINAL_WRITE_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$5,7\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn a_marginal_write_raises_rl_and_reclassifies_the_registers_below_it() {
        let mut control = crate::control::Control::new(MARGINAL_WRITE_MMS, "marginal_write.mms")
            .expect("assembles");
        assert_eq!(control.machine().get_special(SpecialReg::RL), 0);

        // checksmix 0.3.9: "a destination register raises rL before the
        // instruction runs" -- writing $5 while rL = 0 raises rL to 6, then
        // the write itself lands.
        control.step();
        assert_eq!(
            control.machine().get_special(SpecialReg::RL),
            6,
            "SETL $5,7 must raise rL to 6 before it runs"
        );

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(
            control.machine(),
            &continuity,
            control.has_greg_allocations(),
        );
        let class_of = |index: u8| {
            rows.iter().find_map(|row| match row {
                RegisterRow::Register {
                    index: i, class, ..
                } if *i == index => Some(*class),
                _ => None,
            })
        };
        for index in 0u8..=5 {
            assert_eq!(
                class_of(index),
                Some(RegisterClass::Local),
                "${index} must be local"
            );
        }
        for index in 6u8..32 {
            assert_eq!(
                class_of(index),
                Some(RegisterClass::Marginal),
                "${index} must be marginal"
            );
        }
    }

    #[test]
    fn greg_raised_globals_classify_as_global_never_marginal_within_the_frame() {
        let (mmix, has_greg) = assemble(TWO_GREG_MMS, "two_greg.mms");
        assert_eq!(mmix.get_special(SpecialReg::RG), 253);
        let continuity = RegisterContinuity::new();
        let rows = visible_registers(&mmix, &continuity, has_greg);
        let class_of = |rows: &[RegisterRow], index: u8| {
            rows.iter().find_map(|row| match row {
                RegisterRow::Register {
                    index: i, class, ..
                } if *i == index => Some(*class),
                _ => None,
            })
        };
        for index in [253u8, 254, 255] {
            assert_eq!(
                class_of(&rows, index),
                Some(RegisterClass::Global),
                "${index} must be global once rG rises to 253"
            );
        }

        let mut control =
            crate::control::Control::new(GREG_AND_LOCAL_MMS, "local.mms").expect("assembles");
        control.step(); // SETL $40,7 raises rL to 41 before it runs
        assert_eq!(control.machine().get_special(SpecialReg::RG), 254);
        assert_eq!(control.machine().get_special(SpecialReg::RL), 41);
        let continuity = RegisterContinuity::new();
        let rows = visible_registers(
            control.machine(),
            &continuity,
            control.has_greg_allocations(),
        );
        for index in 32u8..=40 {
            assert_eq!(
                class_of(&rows, index),
                Some(RegisterClass::Local),
                "${index} must be local, not global, once rL passes it"
            );
        }
    }

    #[test]
    fn call_mms_rows_never_move_only_class_mark_and_value_change() {
        // Regression guard, exempt from the fail-without-the-change rule:
        // folding the marginal range into one row would split and reflow
        // $0-$31 as rL crosses it. This pins that it never does.
        let mut control = crate::control::Control::new(CALL_MMS, "call.mms").expect("assembles");
        let has_greg = control.has_greg_allocations();
        let continuity = RegisterContinuity::new();

        assert_eq!(control.machine().get_special(SpecialReg::RL), 0);
        let rows_at_load = visible_registers(control.machine(), &continuity, has_greg);

        assert_eq!(
            control.run_chunk(1_000_000),
            crate::control::StepOutcome::Halted
        );
        assert_eq!(control.machine().get_special(SpecialReg::RL), 1);
        let rows_at_halt = visible_registers(control.machine(), &continuity, has_greg);

        fn shape(row: &RegisterRow) -> (bool, u8, u8) {
            match row {
                RegisterRow::Register { index, .. } => (true, *index, *index),
                RegisterRow::ZeroGlobalRange { start, end } => (false, *start, *end),
            }
        }
        let load_shape: Vec<_> = rows_at_load.iter().map(shape).collect();
        let halt_shape: Vec<_> = rows_at_halt.iter().map(shape).collect();
        assert_eq!(
            load_shape, halt_shape,
            "row identity and order must survive rL moving from 0 to 1"
        );

        let class_of = |rows: &[RegisterRow], index: u8| {
            rows.iter().find_map(|row| match row {
                RegisterRow::Register {
                    index: i, class, ..
                } if *i == index => Some(*class),
                _ => None,
            })
        };
        assert_eq!(class_of(&rows_at_load, 0), Some(RegisterClass::Marginal));
        assert_eq!(class_of(&rows_at_halt, 0), Some(RegisterClass::Local));
    }

    #[test]
    fn visible_registers_marks_the_rl_boundary_on_the_first_marginal_row() {
        let (mmix, has_greg) = assemble(NO_GREG_HALT_MMS, "halt.mms");
        let continuity = RegisterContinuity::new();
        let rows = visible_registers(&mmix, &continuity, has_greg);
        let mark_of = |rows: &[RegisterRow], index: u8| {
            rows.iter().find_map(|row| match row {
                RegisterRow::Register { index: i, mark, .. } if *i == index => Some(*mark),
                _ => None,
            })
        };
        assert_eq!(mark_of(&rows, 0), Some(Some(0)), "rL = 0 at load marks $0");

        let mut control = crate::control::Control::new(CALL_MMS, "call.mms").expect("assembles");
        assert_eq!(
            control.run_chunk(1_000_000),
            crate::control::StepOutcome::Halted
        );
        assert_eq!(control.machine().get_special(SpecialReg::RL), 1);
        let continuity = RegisterContinuity::new();
        let rows = visible_registers(
            control.machine(),
            &continuity,
            control.has_greg_allocations(),
        );
        assert_eq!(
            mark_of(&rows, 1),
            Some(Some(1)),
            "rL = 1 after CALL_MMS halts marks $1"
        );

        let mark_count = rows
            .iter()
            .filter(|row| matches!(row, RegisterRow::Register { mark: Some(_), .. }))
            .count();
        assert_eq!(mark_count, 1, "exactly one row carries the mark");
    }

    /// One `GREG` (`rG` = 254 at load), then a local write that raises `rL`
    /// past `$31`, then `PUTI rG,255` -- moves the boundary so `$254`,
    /// visible and sticky as a global register at load, becomes marginal,
    /// while `$41` (the register `rL` actually names) is never written and
    /// never sticky, so it stays hidden. Isolates "first *rendered*
    /// marginal row" from "the row named `rL`".
    const HIDDEN_RL_MMS: &str =
        "\tLOC\t#100\nG1\tGREG\t@\nMain\tSETL\t$40,1\n\tPUTI\trG,255\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn visible_registers_marks_the_first_rendered_marginal_row_not_index_rl() {
        let mut control =
            crate::control::Control::new(HIDDEN_RL_MMS, "hidden_rl.mms").expect("assembles");
        let has_greg = control.has_greg_allocations();
        assert!(has_greg, "fixture must use a real GREG allocation");

        let mut continuity = RegisterContinuity::new();
        continuity.observe(control.machine(), has_greg);
        assert_eq!(
            control.machine().get_special(SpecialReg::RG),
            254,
            "one GREG must allocate exactly $254"
        );

        control.step(); // SETL $40,1 -- raises rL to 41 before it runs
        assert_eq!(control.machine().get_special(SpecialReg::RL), 41);
        continuity.observe(control.machine(), has_greg);

        control.step(); // PUTI rG,255 -- $254 is now marginal, not global
        assert_eq!(control.machine().get_special(SpecialReg::RG), 255);
        continuity.observe(control.machine(), has_greg);

        let rows = visible_registers(control.machine(), &continuity, has_greg);
        let class_of = |index: u8| {
            rows.iter().find_map(|row| match row {
                RegisterRow::Register {
                    index: i, class, ..
                } if *i == index => Some(*class),
                _ => None,
            })
        };
        assert_eq!(
            class_of(41),
            None,
            "$41 is rL's own register -- unwritten and never sticky, so it \
             must not render at all"
        );
        assert_eq!(
            class_of(254),
            Some(RegisterClass::Marginal),
            "$254 stays visible via stickiness, reclassified marginal"
        );

        let mark_of = |index: u8| {
            rows.iter().find_map(|row| match row {
                RegisterRow::Register { index: i, mark, .. } if *i == index => Some(*mark),
                _ => None,
            })
        };
        assert_eq!(
            mark_of(254),
            Some(Some(41)),
            "the mark sits on the first rendered marginal row, naming rL, \
             even though that row's index isn't rL"
        );
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, RegisterRow::Register { mark: Some(_), .. }))
                .count(),
            1,
            "exactly one row carries the mark"
        );
    }

    /// No `GREG` directive (`rG` stays 32), and the one write targets `$31`
    /// -- the last local register below `rG` -- which raises `rL` to 32,
    /// meeting `rG` and leaving the marginal range empty.
    const RL_MEETS_RG_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$31,7\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn visible_registers_marks_no_row_when_none_is_marginal() {
        let mut control =
            crate::control::Control::new(RL_MEETS_RG_MMS, "rl_meets_rg.mms").expect("assembles");
        control.step(); // SETL $31,7 -- raises rL to 32, meeting rG
        assert_eq!(control.machine().get_special(SpecialReg::RL), 32);
        assert_eq!(control.machine().get_special(SpecialReg::RG), 32);

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(
            control.machine(),
            &continuity,
            control.has_greg_allocations(),
        );
        assert!(
            !rows.iter().any(|row| matches!(
                row,
                RegisterRow::Register {
                    class: RegisterClass::Marginal,
                    ..
                }
            )),
            "rl == rg must leave the marginal range empty"
        );
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row, RegisterRow::Register { mark: Some(_), .. })),
            "no row is marginal, so no row may carry the mark"
        );
    }
}
