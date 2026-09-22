//! General and special registers: classification, continuity, and the
//! visible register/special rows.

use std::collections::BTreeSet;

use checksmix::{MMix, SpecialReg};

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
    /// Register `$index`, individually visible under the 3-clause rule
    /// (`sticky || value != 0 || i < rL || i >= rG`).
    Register {
        index: u8,
        value: u64,
        class: RegisterClass,
    },
    /// The `global · rG={rg}` caption, immediately before the first row
    /// whose index is `>= rG`. Absent when no row reaches that threshold
    /// (`rG > 255`).
    GlobalBoundary { rg: u64 },
}

/// The 3-clause visibility rule shared by `visible_registers`'s per-index
/// check and `RegisterContinuity::observe`'s sticky union -- factored out so
/// there is exactly one place this rule can diverge.
fn register_included(index: u8, value: u64, rl: u64, rg: u64) -> bool {
    let addr = u64::from(index);
    value != 0 || addr < rl || addr >= rg
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

    /// Union in every currently-visible index under the 3-clause predicate.
    pub fn observe(&mut self, mmix: &MMix) {
        let rg = mmix.get_special(SpecialReg::RG);
        let rl = mmix.get_special(SpecialReg::RL);
        for i in 0u16..256 {
            let index = i as u8;
            let value = mmix.get_register(index);
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

/// Visible general registers: a register renders when it satisfies
/// [`register_included`] (`sticky || value != 0 || i < rL || i >= rG`),
/// ascending order, a row never moves once shown. `sticky` comes from
/// `continuity`, which remembers any index that has ever satisfied the rule
/// since the last load -- a register `PUSHJ`/`POP`/`PUT rL`/`PUT rG` zeroes
/// back to marginal stays listed, dimmed, rather than vanishing. Each row
/// carries its [`RegisterClass`] under the same `rL`/`rG`. A
/// [`RegisterRow::GlobalBoundary`] caption precedes the first row at or
/// above `rG`.
pub fn visible_registers(mmix: &MMix, continuity: &RegisterContinuity) -> Vec<RegisterRow> {
    let rg = mmix.get_special(SpecialReg::RG);
    let rl = mmix.get_special(SpecialReg::RL);
    let mut rows = Vec::new();
    let mut boundary_emitted = false;

    for i in 0u16..256 {
        let index = i as u8;
        let value = mmix.get_register(index);
        let sticky = continuity.contains(index);

        if sticky || register_included(index, value, rl, rg) {
            emit_global_boundary(&mut rows, &mut boundary_emitted, index, rg);
            rows.push(RegisterRow::Register {
                index,
                value,
                class: register_class(index, rl, rg),
            });
        }
    }

    rows
}

/// Push a [`RegisterRow::GlobalBoundary`] onto `rows` the first time `index`
/// reaches `rg`, per [`visible_registers`]'s one-caption rule. A no-op on
/// every later call, via `boundary_emitted`.
fn emit_global_boundary(
    rows: &mut Vec<RegisterRow>,
    boundary_emitted: &mut bool,
    index: u8,
    rg: u64,
) {
    if !*boundary_emitted && u64::from(index) >= rg {
        rows.push(RegisterRow::GlobalBoundary { rg });
        *boundary_emitted = true;
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

    use crate::machine::fixtures::{CALL_MMS, REVERTING_MARGINAL_MMS, TWO_GREG_MMS};
    use crate::machine::pane::global_boundary_note;

    /// Assemble `source` and load it, unexecuted -- the same shape
    /// `Control::assemble_and_load` uses, restated here so these tests
    /// don't need a `Control`.
    fn assemble(source: &str, filename: &str) -> MMix {
        let mut assembler = MMixAssembler::new(source, filename);
        assembler.parse().expect("test program assembles");
        let mut mmix = MMix::new();
        write_image(&mut mmix, &assembler);
        start_program(&mut mmix, entry_point(&assembler));
        mmix
    }

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
            .filter_map(|row| match row {
                RegisterRow::Register { index, .. } => Some(index),
                RegisterRow::GlobalBoundary { .. } => None,
            })
            .collect();

        // $0-$31 are unwritten (value 0) with rl = 0, so none satisfies
        // `i < rL`; only the `i >= rG` clause admits the three globals.
        assert_eq!(indices, vec![253, 254, 255]);
    }

    /// A `GREG` (raising `rG` above 32) plus a local write past index 32,
    /// so `rL` grows past 32 too -- isolates the `i < rL` clause from the
    /// `i >= rG` clause.
    const GREG_AND_LOCAL_MMS: &str =
        "\tLOC\t#100\nG1\tGREG\t@\nMain\tSETL\t$40,7\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn visible_registers_include_untouched_locals_via_i_lt_rl() {
        let mut mmix = assemble(GREG_AND_LOCAL_MMS, "local.mms");
        assert!(mmix.execute_instruction(), "SETL must execute, not halt");

        let rg = mmix.get_special(SpecialReg::RG);
        let rl = mmix.get_special(SpecialReg::RL);
        assert!(
            rl > 35 && rl < rg,
            "fixture must grow rL strictly between 35 and rG: rl={rl} rg={rg}"
        );

        let continuity = RegisterContinuity::new();
        // $35 is zero-valued, 32 <= 35 < rL -- outside $0-$31, rendered
        // solely through the i < rL clause.
        let has_35 = visible_registers(&mmix, &continuity).iter().any(|row| {
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

        // $0-$4, zero-valued and below $32, render solely through the
        // `i < rL` clause -- deleting that clause is the only way to hide
        // them.
        let mut below_rl = crate::control::Control::new(MARGINAL_WRITE_MMS, "marginal_write.mms")
            .expect("assembles");
        below_rl.step(); // SETL $5,7 raises rL to 6 before it runs
        assert_eq!(below_rl.machine().get_special(SpecialReg::RL), 6);
        let below_rl_continuity = RegisterContinuity::new();
        let below_rl_rows = visible_registers(below_rl.machine(), &below_rl_continuity);
        for index in 0u8..5 {
            assert!(
                below_rl_rows.iter().any(|row| matches!(
                    row,
                    RegisterRow::Register {
                        index: i,
                        value: 0,
                        ..
                    } if *i == index
                )),
                "${index} must render via i < rl"
            );
        }
    }

    /// No `GREG` anywhere, but `PUTI rG,100` moves `rG` at runtime --
    /// checksmix 0.3.13 validates `PUT`/`PUTI rG` against 32..=255 and
    /// >= rL before the store; 100 satisfies both.
    const PUT_RG_MMS: &str = "\tLOC\t#100\nMain\tPUTI\trG,100\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn a_runtime_moved_rg_renders_only_the_registers_at_or_above_it() {
        let mut control =
            crate::control::Control::new(PUT_RG_MMS, "put_rg.mms").expect("assembles");
        assert_eq!(
            control.run_chunk(1_000),
            crate::control::StepOutcome::Halted
        );
        assert_eq!(
            control.machine().get_special(SpecialReg::RG),
            100,
            "PUTI must move rG off its default with no GREG involved"
        );

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(control.machine(), &continuity);

        // $100-$255 render individually via `i >= rG`; $32-$99 render
        // nothing, unwritten and marginal; $0-$31 stay unwritten too, value
        // 0 and rl = 0, so `i < rL` never holds either.
        let individual: Vec<u8> = rows
            .iter()
            .filter_map(|row| match row {
                RegisterRow::Register { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(
            individual,
            (100u16..256).map(|i| i as u8).collect::<Vec<u8>>()
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
            255,
            "no GREG directive: rG is write_image's start value, 255"
        );

        let value255 = control.machine().get_register(255);
        assert_ne!(value255, 0, "fixture must write a nonzero value into $255");

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(control.machine(), &continuity);
        let has_individual_255 = rows.iter().any(|row| {
            matches!(row, RegisterRow::Register { index: 255, value, .. } if *value == value255)
        });
        assert!(
            has_individual_255,
            "$255's nonzero value must render individually"
        );
    }

    #[test]
    fn register_continuity_keeps_a_once_visible_register_after_it_reverts() {
        // $255 always renders individually, so it never witnesses
        // stickiness. `PUTI rL,0` marginalizes $40 back to zero within
        // three instructions, so only per-step observation (not
        // `run_chunk`'s once-per-chunk sampling) ever catches it nonzero --
        // see
        // `sticky_continuity_samples_per_chunk_under_run_and_per_instruction_under_step`.
        let mut control =
            crate::control::Control::new(REVERTING_MARGINAL_MMS, "revert.mms").expect("assembles");
        let mut continuity = RegisterContinuity::new();
        continuity.observe(control.machine());

        let mut saw_nonzero_40 = false;
        while !control.is_halted() {
            control.step();
            saw_nonzero_40 |= control.machine().get_register(40) != 0;
            continuity.observe(control.machine());
        }
        assert!(
            saw_nonzero_40,
            "fixture must write a nonzero value into $40"
        );

        // Reload back to a fresh (all-zero-again) machine, keeping the same
        // continuity tracker: $40 must stay visible, sticky from the
        // earlier observation, even though its value is 0 again.
        control
            .reload(REVERTING_MARGINAL_MMS)
            .expect("still assembles");
        assert_eq!(
            control.machine().get_register(40),
            0,
            "fresh load starts at 0 again"
        );

        let rows = visible_registers(control.machine(), &continuity);
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
        let fresh_rows = visible_registers(control.machine(), &fresh);
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

    /// No `GREG` at all -- the load-time state: `rL = 0`, `rG = 255`.
    /// `MMix::initialize` itself sets `rG` to 32; `write_image` raises it to
    /// 255 for a program with no `GREG` directive (checksmix 0.3.13
    /// `src/mmix.rs`, `src/debugger.rs`).
    const NO_GREG_HALT_MMS: &str = "\tLOC\t#100\nMain\tTRAP\t0,Halt,0\n";

    #[test]
    fn visible_registers_hide_every_marginal_row_at_a_fresh_load() {
        let mmix = assemble(NO_GREG_HALT_MMS, "halt.mms");
        assert_eq!(mmix.get_special(SpecialReg::RL), 0);
        assert_eq!(mmix.get_special(SpecialReg::RG), 255);

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(&mmix, &continuity);

        // $0-$254: rL = 0 and rG = 255, every one marginal and unwritten --
        // no row.
        for index in 0u8..255 {
            assert!(
                !rows.iter().any(|row| matches!(
                    row,
                    RegisterRow::Register { index: i, .. } if *i == index
                )),
                "${index} must have no row at a fresh load"
            );
        }

        let boundary_index = rows
            .iter()
            .position(|row| matches!(row, RegisterRow::GlobalBoundary { rg: 255 }))
            .expect("a GlobalBoundary { rg: 255 } row must precede $255");

        // `start_program`'s start state: $255 holds the entry address, not
        // zero -- an independent oracle, not a readback through `Control`,
        // so this catches the helper drifting from `Control`'s own load
        // path.
        let mut oracle = MMixAssembler::new(NO_GREG_HALT_MMS, "halt.mms");
        oracle.parse().expect("test program assembles");
        let entry = entry_point(&oracle);

        assert!(matches!(
            rows.get(boundary_index + 1),
            Some(RegisterRow::Register {
                index: 255,
                value,
                class: RegisterClass::Global,
            }) if *value == entry
        ));
    }

    #[test]
    fn visible_registers_at_a_fresh_load_are_exactly_the_boundary_and_dollar_255() {
        let mmix = assemble(NO_GREG_HALT_MMS, "halt.mms");
        assert_eq!(mmix.get_special(SpecialReg::RG), 255);
        assert_eq!(mmix.get_special(SpecialReg::RL), 0);

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(&mmix, &continuity);
        assert_eq!(rows.len(), 2, "rows: {rows:?}");
        assert!(matches!(rows[0], RegisterRow::GlobalBoundary { rg: 255 }));
        assert!(matches!(rows[1], RegisterRow::Register { index: 255, .. }));
    }

    #[test]
    fn visible_specials_at_a_fresh_load_show_every_nonzero_start_state_special() {
        // checksmix 0.3.13 starts rK, rT, rTT and rV nonzero; the specials
        // list shows every nonzero special from the moment of load.
        let mmix = assemble(NO_GREG_HALT_MMS, "halt.mms");
        let continuity = SpecialContinuity::new();
        let rows = visible_specials(&mmix, &continuity);

        let value_of = |name: &str| {
            rows.iter()
                .find(|row| row.name == name)
                .map(|row| row.value)
        };
        assert_eq!(value_of("rK"), Some(0xFFFF_FFFF_FFFF_FFFF));
        assert_eq!(value_of("rT"), Some(0x8000_0005_0000_0000));
        assert_eq!(value_of("rTT"), Some(0x8000_0006_0000_0000));
        assert_eq!(value_of("rV"), Some(0x369C_2004_0000_0000));
    }

    #[test]
    fn a_zeroed_dollar_255_still_renders_individually_as_global() {
        // `ViewState::reset` observes the load while $255 holds the entry
        // address (`start_program`), which would make $255 sticky and hide
        // whether it renders on its own merit -- render through a fresh
        // continuity instead, not through `ViewState`.
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
        let rows = visible_registers(control.machine(), &continuity);
        let reg255 = rows
            .iter()
            .find(|row| matches!(row, RegisterRow::Register { index: 255, .. }))
            .expect("$255 must render individually even at zero");
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
    fn visible_registers_classify_call_mms_after_return_as_local_with_the_rest_hidden() {
        let mut control = crate::control::Control::new(CALL_MMS, "call.mms").expect("assembles");
        assert_eq!(
            control.run_chunk(1_000_000),
            crate::control::StepOutcome::Halted
        );
        assert_eq!(control.machine().get_special(SpecialReg::RL), 1);

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(control.machine(), &continuity);

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
                None,
                "${index} is marginal and unwritten once rL falls back to \
                 1 -- with a fresh continuity it has no row"
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
        let rows = visible_registers(control.machine(), &continuity);
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
        // Marginal and unwritten, so none of register_included's clauses
        // holds and none renders. Restoring `index < 32 ||` in
        // `visible_registers` makes this fail.
        for index in 6u8..32 {
            assert_eq!(class_of(index), None, "${index} must have no row");
        }
    }

    #[test]
    fn greg_raised_globals_classify_as_global_never_marginal_within_the_frame() {
        let mmix = assemble(TWO_GREG_MMS, "two_greg.mms");
        assert_eq!(mmix.get_special(SpecialReg::RG), 253);
        let continuity = RegisterContinuity::new();
        let rows = visible_registers(&mmix, &continuity);
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
        let rows = visible_registers(control.machine(), &continuity);
        for index in 32u8..=40 {
            assert_eq!(
                class_of(&rows, index),
                Some(RegisterClass::Local),
                "${index} must be local, not global, once rL passes it"
            );
        }
    }

    #[test]
    fn call_mms_rows_never_move_only_class_and_value_change() {
        // Regression guard, exempt from the fail-without-the-change rule:
        // dropping stickiness would let a row vanish and reappear as rL/rG
        // move mid-run. This pins that it never does.
        let mut control = crate::control::Control::new(CALL_MMS, "call.mms").expect("assembles");
        let mut continuity = RegisterContinuity::new();
        continuity.observe(control.machine());

        let mut snapshots = vec![visible_registers(control.machine(), &continuity)];
        for _ in 0..16 {
            if control.is_halted() {
                break;
            }
            control.step();
            continuity.observe(control.machine());
            snapshots.push(visible_registers(control.machine(), &continuity));
        }
        assert!(control.is_halted(), "the run must reach a halt");

        fn individual_indices(rows: &[RegisterRow]) -> Vec<u8> {
            rows.iter()
                .filter_map(|row| match row {
                    RegisterRow::Register { index, .. } => Some(*index),
                    _ => None,
                })
                .collect()
        }

        let halt_indices = individual_indices(snapshots.last().expect("at least one snapshot"));
        for rows in &snapshots {
            let indices = individual_indices(rows);
            assert!(
                indices.windows(2).all(|pair| pair[0] < pair[1]),
                "individual indices must ascend within one snapshot: {indices:?}"
            );
            for index in &indices {
                assert!(
                    halt_indices.contains(index),
                    "${index} rendered mid-run must still render at halt"
                );
            }
        }

        let class_of = |rows: &[RegisterRow], index: u8| {
            rows.iter().find_map(|row| match row {
                RegisterRow::Register {
                    index: i, class, ..
                } if *i == index => Some(*class),
                _ => None,
            })
        };
        assert_eq!(
            class_of(snapshots.last().unwrap(), 0),
            Some(RegisterClass::Local),
            "$0 must be local at halt"
        );
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
    fn a_sticky_register_stays_marginal_visible_while_an_unwritten_one_hides() {
        let mut control =
            crate::control::Control::new(HIDDEN_RL_MMS, "hidden_rl.mms").expect("assembles");

        let mut continuity = RegisterContinuity::new();
        continuity.observe(control.machine());
        assert_eq!(
            control.machine().get_special(SpecialReg::RG),
            254,
            "one GREG must allocate exactly $254"
        );

        control.step(); // SETL $40,1 -- raises rL to 41 before it runs
        assert_eq!(control.machine().get_special(SpecialReg::RL), 41);
        continuity.observe(control.machine());

        control.step(); // PUTI rG,255 -- $254 is now marginal, not global
        assert_eq!(control.machine().get_special(SpecialReg::RG), 255);
        continuity.observe(control.machine());

        let rows = visible_registers(control.machine(), &continuity);
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
    }

    /// No `GREG` directive (`rG` stays 255), and the one write targets
    /// `$254` -- the last local register below `rG` -- which raises `rL` to
    /// 255, meeting `rG` and leaving the marginal range empty.
    const RL_MEETS_RG_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$254,7\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn visible_registers_render_no_marginal_row_when_rl_meets_rg() {
        let mut control =
            crate::control::Control::new(RL_MEETS_RG_MMS, "rl_meets_rg.mms").expect("assembles");
        control.step(); // SETL $254,7 -- raises rL to 255, meeting rG
        assert_eq!(control.machine().get_special(SpecialReg::RL), 255);
        assert_eq!(control.machine().get_special(SpecialReg::RG), 255);

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(control.machine(), &continuity);
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
    }

    #[test]
    fn a_sticky_register_zeroed_marginal_by_a_pop_stays_listed() {
        // The PUSHJ's window slide (push_frame) zeroes $2, and the POP
        // (pop_frame) zeroes $1, leaving rL = 1: both are marginal and
        // zero at halt, but sticky from rendering individually mid-call.
        let mut control = crate::control::Control::new(CALL_MMS, "call.mms").expect("assembles");
        let mut continuity = RegisterContinuity::new();
        continuity.observe(control.machine());

        for _ in 0..16 {
            if control.is_halted() {
                break;
            }
            control.step();
            continuity.observe(control.machine());
        }
        assert!(control.is_halted(), "the run must reach a halt");
        assert_eq!(control.machine().get_special(SpecialReg::RL), 1);
        assert_eq!(control.machine().get_register(1), 0);
        assert_eq!(control.machine().get_register(2), 0);

        let rows = visible_registers(control.machine(), &continuity);
        let class_of = |index: u8| {
            rows.iter().find_map(|row| match row {
                RegisterRow::Register {
                    index: i,
                    value,
                    class,
                } if *i == index => Some((*value, *class)),
                _ => None,
            })
        };
        assert_eq!(
            class_of(1),
            Some((0, RegisterClass::Marginal)),
            "$1 must stay listed, marginal and zero, after the POP"
        );
        assert_eq!(
            class_of(2),
            Some((0, RegisterClass::Marginal)),
            "$2 must stay listed, marginal and zero, after the window slide"
        );

        // With a fresh continuity, neither has a row: unwritten and
        // marginal, $1/$2 satisfy no clause of register_included.
        let fresh = RegisterContinuity::new();
        let fresh_rows = visible_registers(control.machine(), &fresh);
        assert!(
            !fresh_rows
                .iter()
                .any(|row| matches!(row, RegisterRow::Register { index: 1, .. })),
            "$1 must have no row under a fresh continuity"
        );
        assert!(
            !fresh_rows
                .iter()
                .any(|row| matches!(row, RegisterRow::Register { index: 2, .. })),
            "$2 must have no row under a fresh continuity"
        );
    }

    #[test]
    fn the_global_boundary_caption_sits_immediately_before_the_first_row_at_or_above_rg() {
        let mmix = assemble(TWO_GREG_MMS, "two_greg.mms");
        assert_eq!(mmix.get_special(SpecialReg::RG), 253);

        let continuity = RegisterContinuity::new();
        let rows = visible_registers(&mmix, &continuity);

        let boundaries: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| matches!(row, RegisterRow::GlobalBoundary { .. }).then_some(i))
            .collect();
        assert_eq!(boundaries.len(), 1, "exactly one caption");
        let at = boundaries[0];
        assert_eq!(rows[at], RegisterRow::GlobalBoundary { rg: 253 });
        assert_eq!(global_boundary_note(253), "global \u{b7} rG=253");
        assert!(
            matches!(rows[at + 1], RegisterRow::Register { index: 253, .. }),
            "the caption must immediately precede $253"
        );

        // HIDDEN_RL_MMS after PUTI rG,255: the caption sits between the
        // sticky-marginal $254 and $255.
        let mut control =
            crate::control::Control::new(HIDDEN_RL_MMS, "hidden_rl.mms").expect("assembles");
        let mut continuity = RegisterContinuity::new();
        continuity.observe(control.machine());
        control.step(); // SETL $40,1
        continuity.observe(control.machine());
        control.step(); // PUTI rG,255
        assert_eq!(control.machine().get_special(SpecialReg::RG), 255);
        continuity.observe(control.machine());

        let rows = visible_registers(control.machine(), &continuity);
        let pos_254 = rows
            .iter()
            .position(|row| matches!(row, RegisterRow::Register { index: 254, .. }))
            .expect("$254 must stay listed, sticky-marginal");
        assert_eq!(rows[pos_254 + 1], RegisterRow::GlobalBoundary { rg: 255 });
        assert!(matches!(
            rows[pos_254 + 2],
            RegisterRow::Register { index: 255, .. }
        ));
    }
}
