//! Cross-render view state: register/special continuity and the
//! pause-boundary diff snapshot.

use std::collections::{BTreeMap, BTreeSet};

use crate::control::Control;
use crate::machine::memory::{MemoryRow, memory_rows, memory_runs};
use crate::machine::registers::{
    RegisterContinuity, RegisterRow, SpecialContinuity, SpecialRegisterRow, visible_registers,
    visible_specials,
};

/// Every individually-rendered or collapsed index's value across `rows`. An
/// index absent from the result is not itself unknown: per the visibility
/// rules governing what `rows` holds, an index that renders neither
/// individually nor via the collapse necessarily has value `0` -- the
/// 3-clause predicate would have rendered it individually otherwise. Callers
/// comparing two snapshots (e.g. a diff) must treat an absent index as `0`,
/// not as unknown.
fn register_value_map(rows: &[RegisterRow]) -> BTreeMap<u8, u64> {
    let mut map = BTreeMap::new();
    for row in rows {
        match row {
            RegisterRow::Register { index, value, .. } => {
                map.insert(*index, *value);
            }
            RegisterRow::ZeroGlobalRange { start, end } => {
                for i in *start..=*end {
                    map.insert(i, 0);
                }
            }
            RegisterRow::GlobalBoundary { .. } => {}
        }
    }
    map
}

/// The register indices whose value differs between `prev` and `curr`. An
/// index absent from `prev` (not yet individually visible or collapsed --
/// see [`register_value_map`]) is treated as value `0`, its actual value per
/// the visibility rules, so an index's first appearance at a nonzero value
/// -- e.g. a sticky register or a `GREG`-widened range becoming individually
/// visible -- is flagged. An index that merely appears or disappears with an
/// unchanged value (moving in or out of the collapse) is unaffected.
pub fn diff_registers(prev: &[RegisterRow], curr: &[RegisterRow]) -> BTreeSet<u8> {
    let prev_map = register_value_map(prev);
    let curr_map = register_value_map(curr);
    curr_map
        .into_iter()
        .filter(|(index, value)| prev_map.get(index).copied().unwrap_or(0) != *value)
        .map(|(index, _)| index)
        .collect()
}

fn special_value_map(rows: &[SpecialRegisterRow]) -> BTreeMap<&str, u64> {
    rows.iter()
        .map(|row| (row.name.as_str(), row.value))
        .collect()
}

/// The special-register names whose value differs between `prev` and
/// `curr`, same rule as [`diff_registers`]: a name absent from `prev` is
/// treated as value `0` -- the only value a non-pinned special can have
/// before it first satisfies the sticky predicate -- not as unknown, so a
/// special's first nonzero appearance is flagged.
pub fn diff_specials(prev: &[SpecialRegisterRow], curr: &[SpecialRegisterRow]) -> BTreeSet<String> {
    let prev_map = special_value_map(prev);
    curr.iter()
        .filter(|row| prev_map.get(row.name.as_str()).copied().unwrap_or(0) != row.value)
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

/// Everything the machine pane renders from that isn't machine state:
/// which registers and specials stay visible ([`RegisterContinuity`],
/// [`SpecialContinuity`]), the previous pause boundary's snapshot, and the
/// changed-since-that-boundary sets. `App` owns one and drives it from its
/// `Msg` handlers.
///
/// Plain and `Control`-driven rather than Yew-coupled -- the same
/// extraction `Control` itself is, for the same reason (`AGENTS.md`'s rule
/// that logic not needing browser APIs stays host-testable). One `&Control`
/// supplies everything these methods need: `machine()`, `labels()`, and
/// `has_greg_allocations()`.
#[derive(Debug, Default)]
pub struct ViewState {
    register_continuity: RegisterContinuity,
    special_continuity: SpecialContinuity,
    /// The registers/specials/memory as of the previous pause boundary --
    /// what the `diff_*` functions compare the current state against.
    /// Seeded by [`ViewState::reset`], advanced only by
    /// [`ViewState::record_pause_boundary`].
    prev_registers: Vec<RegisterRow>,
    prev_specials: Vec<SpecialRegisterRow>,
    prev_memory: Vec<MemoryRow>,
    changed_registers: BTreeSet<u8>,
    changed_specials: BTreeSet<String>,
    changed_memory: BTreeSet<u64>,
}

impl ViewState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Union the currently-visible registers and specials into both sticky
    /// sets. Sampling, not tracking: a register that goes nonzero and back
    /// to zero entirely between two calls is never seen. `App` calls this
    /// at every pause boundary and every chunk yield, which is what
    /// `docs/layout-spec.md`'s Sticky rule documents.
    pub fn observe(&mut self, control: &Control) {
        self.register_continuity
            .observe(control.machine(), control.has_greg_allocations());
        self.special_continuity.observe(control.machine());
    }

    /// The register, special, and memory rows to render, computed fresh
    /// from `control` against the current sticky sets. The render path and
    /// both snapshot points share this, so a rendered row and a diffed row
    /// can never be computed different ways.
    pub fn machine_rows(
        &self,
        control: &Control,
    ) -> (Vec<RegisterRow>, Vec<SpecialRegisterRow>, Vec<MemoryRow>) {
        let mmix = control.machine();
        let registers = visible_registers(
            mmix,
            &self.register_continuity,
            control.has_greg_allocations(),
        );
        let specials = visible_specials(mmix, &self.special_continuity);
        let memory = memory_rows(&memory_runs(mmix, control.labels()));
        (registers, specials, memory)
    }

    /// Recompute the changed-since-last-pause sets against the previous
    /// pause boundary's snapshot, then advance the snapshot to the current
    /// state -- called only at an actual pause boundary (a Step that
    /// executed, a Next's, Run's, or Continue's terminal outcome, or an
    /// explicit Interrupt), never on an intermediate chunk repaint.
    pub fn record_pause_boundary(&mut self, control: &Control) {
        let (registers, specials, memory) = self.machine_rows(control);

        self.changed_registers = diff_registers(&self.prev_registers, &registers);
        self.changed_specials = diff_specials(&self.prev_specials, &specials);
        self.changed_memory = diff_memory(&self.prev_memory, &memory);

        self.prev_registers = registers;
        self.prev_specials = specials;
        self.prev_memory = memory;
    }

    /// Establish a fresh baseline after a successful load/reload: drop both
    /// continuity trackers, seed them from the freshly loaded machine (so a
    /// register visible only at load isn't lost on the very first step),
    /// and capture the fresh state as the "previous" snapshot so the first
    /// real pause boundary diffs against actual fresh-load values, not an
    /// empty snapshot that would flag every already-nonzero register as
    /// changed. Not a pause boundary itself -- `changed_*` stays empty.
    pub fn reset(&mut self, control: &Control) {
        self.register_continuity = RegisterContinuity::new();
        self.special_continuity = SpecialContinuity::new();
        self.observe(control);

        let (registers, specials, memory) = self.machine_rows(control);
        self.prev_registers = registers;
        self.prev_specials = specials;
        self.prev_memory = memory;

        self.clear_changed();
    }

    /// Clear the changed-since-last-pause sets -- the moment a Run,
    /// Continue, or Next resumes advancing, per `docs/layout-spec.md`'s
    /// Highlights §3.
    pub fn clear_changed(&mut self) {
        self.changed_registers.clear();
        self.changed_specials.clear();
        self.changed_memory.clear();
    }

    pub fn changed_registers(&self) -> &BTreeSet<u8> {
        &self.changed_registers
    }

    pub fn changed_specials(&self) -> &BTreeSet<String> {
        &self.changed_specials
    }

    pub fn changed_memory(&self) -> &BTreeSet<u64> {
        &self.changed_memory
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use checksmix::SpecialReg;

    use crate::machine::fixtures::{CALL_MMS, REVERTING_GLOBAL_MMS, TWO_GREG_MMS};
    use crate::machine::memory::{MEMORY_ROW_WIDTH, Segment};
    use crate::machine::registers::{PINNED_SPECIALS, RegisterClass};

    /// Writes a register (so `rL` grows too, changing a special), then
    /// stores a byte into the data segment -- a real memory write, unlike
    /// `CALL_MMS`, which only ever touches registers and specials.
    const STORE_MMS: &str = "\tLOC\tData_Segment\n\tGREG\t@\nText\tBYTE\t\"ab\",0\n\tLOC\t#100\nMain\tLDA\t$1,Text\n\tSETL\t$2,88\n\tSTB\t$2,$1,0\n\tTRAP\t0,Halt,0\n";

    /// `rZ` is not one of the always-shown `PINNED_SPECIALS`, so it only
    /// ever renders via the sticky set -- isolates the special-register
    /// half of `ViewState::observe` from the register half other
    /// `ViewState` tests already cover. `rQ` would fit the same role but is
    /// read-only in user mode under checksmix 0.3.10; `rZ` is not.
    const PUT_RZ_MMS: &str = "\tLOC\t#100\nMain\tPUTI\trZ,7\n\tPUTI\trZ,0\n\tTRAP\t0,Halt,0\n";

    /// Whether `$index` renders individually under `view`'s current sticky
    /// set -- the question every `ViewState` test below actually asks.
    fn renders_individually(
        view: &ViewState,
        control: &crate::control::Control,
        index: u8,
    ) -> bool {
        let (registers, _, _) = view.machine_rows(control);
        registers
            .iter()
            .any(|row| matches!(row, RegisterRow::Register { index: i, .. } if *i == index))
    }

    #[test]
    fn view_state_records_what_changed_across_a_pause_boundary() {
        let mut control = crate::control::Control::new(CALL_MMS, "call.mms").expect("assembles");
        let mut view = ViewState::new();
        view.reset(&control);
        assert!(
            view.changed_registers().is_empty(),
            "a fresh baseline is not a pause boundary"
        );

        assert_eq!(
            control.run_chunk(1_000_000),
            crate::control::StepOutcome::Halted
        );
        view.observe(&control);
        view.record_pause_boundary(&control);

        // Under checksmix 0.3.9's register-stack rule, CALL_MMS's `POP 1,0`
        // returns one value into the hole PUSHJ $0 left and marginalizes
        // the rest: the program ends with $0 = 42, $255 = 42, rL = 1 --
        // and $1, $2 (40 and 2 mid-call) zeroed back out, since both sit
        // above the new rL. rL still grows past its load-time value too.
        assert!(
            view.changed_registers().contains(&0) && view.changed_registers().contains(&255),
            "registers the run wrote must be flagged: {:?}",
            view.changed_registers()
        );
        assert!(
            view.changed_specials().contains("rL"),
            "rL grew across the run: {:?}",
            view.changed_specials()
        );
        assert_eq!(
            control.machine().get_register(1),
            0,
            "the new POP rule must marginalize $1 back to zero"
        );
        assert_eq!(
            control.machine().get_register(2),
            0,
            "the new POP rule must marginalize $2 back to zero"
        );

        // A second boundary with nothing executed in between diffs against
        // the snapshot the first one just advanced to, so nothing changed.
        view.record_pause_boundary(&control);
        assert!(view.changed_registers().is_empty());
        assert!(view.changed_specials().is_empty());
    }

    #[test]
    fn view_state_clear_changed_empties_every_set() {
        let mut control = crate::control::Control::new(STORE_MMS, "store.mms").expect("assembles");
        let mut view = ViewState::new();
        view.reset(&control);
        assert_eq!(
            control.run_chunk(1_000_000),
            crate::control::StepOutcome::Halted
        );
        view.observe(&control);
        view.record_pause_boundary(&control);
        assert!(
            !view.changed_registers().is_empty()
                && !view.changed_specials().is_empty()
                && !view.changed_memory().is_empty(),
            "fixture assumption: STORE_MMS's run changes a register, a \
             special (rL grows), and memory (the STB write): {:?} {:?} {:?}",
            view.changed_registers(),
            view.changed_specials(),
            view.changed_memory()
        );

        view.clear_changed();
        assert!(view.changed_registers().is_empty());
        assert!(view.changed_specials().is_empty());
        assert!(view.changed_memory().is_empty());
    }

    #[test]
    fn view_state_reset_drops_the_sticky_set_from_the_previous_load() {
        // $255 always renders individually, so it can no longer witness
        // stickiness; $40 reverts to zero within the run and needs
        // per-step observation to catch it nonzero -- see
        // register_continuity_keeps_a_once_visible_register_after_it_reverts.
        let mut control =
            crate::control::Control::new(REVERTING_GLOBAL_MMS, "revert.mms").expect("assembles");
        let mut view = ViewState::new();
        view.reset(&control);

        let mut saw_nonzero_40 = false;
        while !control.is_halted() {
            control.step();
            saw_nonzero_40 |= control.machine().get_register(40) != 0;
            view.observe(&control);
        }
        assert!(
            saw_nonzero_40,
            "fixture must write a nonzero value into $40"
        );
        assert_eq!(
            control.machine().get_register(40),
            0,
            "the fixture reverts $40 to 0 before halting"
        );

        // Reload alone leaves the sticky set intact -- $40 is back to zero
        // but keeps its row, which is the whole point of continuity.
        control
            .reload(REVERTING_GLOBAL_MMS)
            .expect("still assembles");
        assert_eq!(control.machine().get_register(40), 0);
        assert!(
            renders_individually(&view, &control, 40),
            "$40 must still be sticky before the reset"
        );

        // Deleting either continuity-clearing line in `reset` leaves $40
        // sticky here, across a load it was never visible in.
        view.reset(&control);
        assert!(
            !renders_individually(&view, &control, 40),
            "reset must drop the previous load's sticky set"
        );
    }

    #[test]
    fn view_state_reset_seeds_the_diff_baseline_from_the_fresh_load() {
        // `G2 GREG @` initializes $253 to a nonzero address at load time,
        // before anything executes.
        let control =
            crate::control::Control::new(TWO_GREG_MMS, "two_greg.mms").expect("assembles");
        assert_ne!(
            control.machine().get_register(253),
            0,
            "the fixture must load with a nonzero register"
        );

        let mut view = ViewState::new();
        view.reset(&control);
        view.record_pause_boundary(&control);

        // Without the snapshot seeding in `reset`, the first pause boundary
        // diffs against an empty snapshot and flags every already-nonzero
        // register as freshly changed.
        assert!(
            view.changed_registers().is_empty(),
            "a load-time value is not a change: {:?}",
            view.changed_registers()
        );
    }

    #[test]
    fn view_state_reset_seeds_register_sticky_set_from_the_fresh_load() {
        // `G2 GREG @` initializes $253 to a nonzero address at load time,
        // before anything executes. For this fixture $253 stays >= rG for
        // the whole run (nothing moves rG afterward), so it would also
        // render individually via `register_included` alone -- checking
        // render output wouldn't isolate what `reset`'s own seeding
        // `observe` call does here. Check the sticky set's membership
        // directly instead.
        let control =
            crate::control::Control::new(TWO_GREG_MMS, "two_greg.mms").expect("assembles");
        assert_ne!(
            control.machine().get_register(253),
            0,
            "fixture must load with a nonzero register"
        );

        let mut view = ViewState::new();
        view.reset(&control);

        assert!(
            view.register_continuity.contains(253),
            "reset's own seeding observe must mark a load-time-nonzero \
             register sticky, not just leave it visible by coincidence"
        );
    }

    #[test]
    fn view_state_observe_tracks_special_register_continuity_too() {
        assert!(
            !PINNED_SPECIALS.contains(&SpecialReg::RZ),
            "fixture assumption"
        );
        let mut control =
            crate::control::Control::new(PUT_RZ_MMS, "put_rz.mms").expect("assembles");
        let mut view = ViewState::new();
        view.reset(&control);

        control.step(); // PUTI rZ,7 -- rZ now nonzero
        view.observe(&control);
        control.step(); // PUTI rZ,0 -- rZ reverts to zero

        let (_, specials, _) = view.machine_rows(&control);
        assert!(
            specials
                .iter()
                .any(|row| row.name == "rZ" && row.value == 0),
            "ViewState::observe must wire through to SpecialContinuity::observe \
             so rZ stays visible after reverting to zero: {specials:?}"
        );
    }

    #[test]
    fn sticky_continuity_samples_per_chunk_under_run_and_per_instruction_under_step() {
        // Observed once, after the whole chunk -- how `Msg::Run` samples.
        let mut chunked =
            crate::control::Control::new(REVERTING_GLOBAL_MMS, "revert.mms").expect("assembles");
        let mut chunk_view = ViewState::new();
        chunk_view.reset(&chunked);
        assert_eq!(
            chunked.run_chunk(crate::control::CHUNK_BUDGET),
            crate::control::StepOutcome::Halted
        );
        chunk_view.observe(&chunked);

        // Observed after every instruction -- how `Msg::Step` samples.
        let mut stepped =
            crate::control::Control::new(REVERTING_GLOBAL_MMS, "revert.mms").expect("assembles");
        let mut step_view = ViewState::new();
        step_view.reset(&stepped);
        for _ in 0..16 {
            if stepped.is_halted() {
                break;
            }
            stepped.step();
            step_view.observe(&stepped);
        }
        assert!(stepped.is_halted(), "the stepped run must reach the halt");

        // Identical programs, identical end states: the only difference is
        // when the visible set was sampled.
        assert_eq!(chunked.machine().get_register(40), 0);
        assert_eq!(stepped.machine().get_register(40), 0);
        assert_eq!(
            chunked.machine().get_special(SpecialReg::RL),
            stepped.machine().get_special(SpecialReg::RL),
            "PUTI rL,0 leaves rL = 0 regardless of chunk granularity"
        );

        assert!(
            !renders_individually(&chunk_view, &chunked, 40),
            "chunk-granularity sampling cannot see a value that reverted \
             inside the chunk"
        );
        assert!(
            renders_individually(&step_view, &stepped, 40),
            "per-instruction sampling catches it, and stickiness keeps the row"
        );
    }

    #[test]
    fn diff_registers_flags_only_indices_whose_value_differs() {
        let prev = vec![
            RegisterRow::Register {
                index: 1,
                value: 5,
                class: RegisterClass::Local,
            },
            RegisterRow::Register {
                index: 2,
                value: 9,
                class: RegisterClass::Local,
            },
            RegisterRow::ZeroGlobalRange {
                start: 32,
                end: 254,
            },
        ];
        let curr = vec![
            RegisterRow::Register {
                index: 1,
                value: 5,
                class: RegisterClass::Local,
            }, // unchanged
            RegisterRow::Register {
                index: 2,
                value: 10,
                class: RegisterClass::Local,
            }, // changed
            // Newly individually visible (moved out of the collapse), but
            // still zero -- must not be flagged.
            RegisterRow::Register {
                index: 40,
                value: 0,
                class: RegisterClass::Global,
            },
        ];

        let diff = diff_registers(&prev, &curr);
        assert_eq!(diff, BTreeSet::from([2]));
    }

    #[test]
    fn diff_registers_flags_a_nonzero_value_s_first_appearance() {
        // Index 50 is absent from prev entirely -- not individually
        // rendered, and not covered by the collapse (e.g. rG != 32, so the
        // no-GREG collapse gate doesn't fire for it). Per the visibility
        // rules, an index absent this way was value 0; becoming sticky at a
        // nonzero value is exactly the transition a user watching the diff
        // cares about, and must be flagged, not treated as unknown/skip.
        let prev = vec![RegisterRow::Register {
            index: 1,
            value: 5,
            class: RegisterClass::Local,
        }];
        let curr = vec![
            RegisterRow::Register {
                index: 1,
                value: 5,
                class: RegisterClass::Local,
            }, // unchanged
            RegisterRow::Register {
                index: 50,
                value: 7,
                class: RegisterClass::Global,
            }, // first appearance, nonzero
        ];

        let diff = diff_registers(&prev, &curr);
        assert_eq!(diff, BTreeSet::from([50]));
    }

    #[test]
    fn diff_registers_treats_an_absent_index_as_zero_not_unknown() {
        // Index 50 is again absent from prev, but this time its first
        // appearance in curr is at value 0 -- e.g. a global register
        // entering visibility via `i >= rG` while still unwritten. If an
        // absent index were treated as some other sentinel (unknown, or a
        // nonzero placeholder) rather than the actual value 0 the
        // visibility rules guarantee, this would wrongly flag it as
        // changed. Pins the specific default, not just "isn't skipped" --
        // `diff_registers_flags_a_nonzero_value_s_first_appearance` above
        // only proves the latter.
        let prev = vec![RegisterRow::Register {
            index: 1,
            value: 5,
            class: RegisterClass::Local,
        }];
        let curr = vec![
            RegisterRow::Register {
                index: 1,
                value: 5,
                class: RegisterClass::Local,
            }, // unchanged
            RegisterRow::Register {
                index: 50,
                value: 0,
                class: RegisterClass::Global,
            }, // first appearance, still zero
        ];

        let diff = diff_registers(&prev, &curr);
        assert_eq!(diff, BTreeSet::new());
    }

    #[test]
    fn diff_specials_flags_a_nonzero_value_s_first_appearance() {
        // "rX" is absent from prev -- not yet individually visible. Per the
        // visibility rules its prior value was 0; a later nonzero value
        // (the sticky transition) must be flagged.
        let prev = vec![SpecialRegisterRow {
            name: "rJ".to_string(),
            value: 5,
        }];
        let curr = vec![
            SpecialRegisterRow {
                name: "rJ".to_string(),
                value: 5,
            }, // unchanged
            SpecialRegisterRow {
                name: "rX".to_string(),
                value: 3,
            }, // first appearance, nonzero
        ];

        let diff = diff_specials(&prev, &curr);
        assert_eq!(diff, BTreeSet::from(["rX".to_string()]));
    }

    #[test]
    fn diff_specials_treats_an_absent_name_as_zero_not_unknown() {
        // Same pin as diff_registers_treats_an_absent_index_as_zero_not_unknown,
        // for specials: "rX"'s first appearance in curr is at value 0, so it
        // must not be flagged -- proving the default is specifically 0, not
        // just "not skipped".
        let prev = vec![SpecialRegisterRow {
            name: "rJ".to_string(),
            value: 5,
        }];
        let curr = vec![
            SpecialRegisterRow {
                name: "rJ".to_string(),
                value: 5,
            }, // unchanged
            SpecialRegisterRow {
                name: "rX".to_string(),
                value: 0,
            }, // first appearance, still zero
        ];

        let diff = diff_specials(&prev, &curr);
        assert_eq!(diff, BTreeSet::new());
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

    #[test]
    fn the_changed_highlight_survives_the_pop_marginalizing_a_register() {
        // CALL_MMS steps as: SETL $1,40; SETL $2,2; PUSHJ $0,AddFunc, whose
        // window slide moves the caller's $2 into the callee's $1 (value
        // 2); ADDU $0,$0,$1; POP 1,0, which marginalizes the caller's frame
        // back to rL = 1 and zeroes the caller's $1 from 2 to 0 -- the only
        // register that step changes; SET $255,$0; TRAP. $1's 2 -> 0
        // transition below lands on that POP step.
        let mut control = crate::control::Control::new(CALL_MMS, "call.mms").expect("assembles");
        let mut view = ViewState::new();
        view.reset(&control);

        let mut pop_boundary_changed = None;
        for _ in 0..16 {
            if control.is_halted() {
                break;
            }
            let value_before = control.machine().get_register(1);
            control.step();
            view.observe(&control);
            view.record_pause_boundary(&control);
            let value_after = control.machine().get_register(1);
            if value_before == 2 && value_after == 0 {
                pop_boundary_changed = Some(view.changed_registers().clone());
            }
        }
        assert!(control.is_halted(), "the run must reach a halt");

        let changed = pop_boundary_changed.expect("the POP step (2 -> 0 on $1) must occur");
        assert_eq!(
            changed,
            BTreeSet::from([1]),
            "the POP must change only $1: {changed:?}"
        );
    }
}
