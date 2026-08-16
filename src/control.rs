//! Execution model and control-pane UI: a run only happens when the user
//! asks for one, and can always be stopped.
//!
//! [`Control`] is the plain, host-testable core (`AGENTS.md`'s rule that
//! logic not needing browser APIs stays plain): it owns the loaded [`MMix`]
//! and its [`MMixAssembler`] across steps, the breakpoint line set, and
//! whether a run is in flight. No Yew, no browser API, no timers -- `cargo
//! test` drives it directly.
//!
//! [`ControlBar`] is the Yew-facing button bar. The chunked run loop itself
//! lives in `main.rs`'s `App`, which is what holds the machine across
//! renders (see that module) and therefore what decides when to call
//! [`Control::run_chunk`] or [`Control::step_over_chunk`] and when to yield
//! via [`yield_to_event_loop`].

use std::collections::{BTreeSet, HashMap};

use checksmix::{MMix, MMixAssembler, entry_point, write_image};
use gloo_timers::callback::Timeout;
use yew::prelude::*;

/// The MMIX text/data segment boundary: the top three address bits select
/// the segment (0 text, 1 data, 2 pool, 3 stack), so any address at or past
/// this is data, not code. The program counter never enters data, so a
/// breakpoint resolved to such an address could never fire. checksmix uses
/// this same constant internally (`SEGMENT_BOUNDARY` in its `debugger.rs`,
/// `DATA_SEGMENT_START` in its `mmixal.rs`) but doesn't expose it; it's a
/// stable MMIX architectural boundary, safe to restate here.
const DATA_SEGMENT_START: u64 = 0x2000_0000_0000_0000;

/// Instructions per chunk: the interrupt granularity for Run and a chunked
/// Step Over, since either can only be stopped at a chunk boundary. Not a
/// throughput knob.
///
/// Chunk wall-clock cost depends on opcode mix and memory-access pattern,
/// not just instruction count, so this is tuned to the heaviest reproduced
/// mix, not the cheapest. Measured under `wasm32-unknown-unknown` at
/// `opt-level = 'z'` (the release profile playmmix builds) on V8 (Node; the
/// same wasm engine Chrome embeds), running 3,000,000 instructions of each
/// mix: a tight `ADDU`/`JMP` loop ~268 ms; a mix that touches memory
/// (`STOU`/`LDOU`/`MULU`/`DIVU`) ~508 ms; a loop that keeps writing to fresh
/// addresses, growing checksmix's `HashMap<u64,u8>`-backed memory, ~735 ms
/// -- the worst case, since a growing hash map is the most expensive access
/// pattern this interpreter has. Instruction cost is roughly linear in count
/// for a fixed mix, so scaling down to 1,000,000 brings the growing-heap
/// case to ~245 ms (the actual worst-case Stop latency this budget is tuned
/// for) while dropping the tight-loop case to ~90 ms.
pub const CHUNK_BUDGET: usize = 1_000_000;

/// Why [`Control::step`], [`Control::step_over_chunk`], or
/// [`Control::run_chunk`] stopped. Close to [`checksmix::Stop`] but adds
/// `Advanced`: the operation completed without being interrupted by a halt,
/// a breakpoint, or the budget running out -- a plain Step's one
/// instruction, or a Step Over reaching the pre-call depth (whether that
/// took one chunk or several). `run_chunk` never returns `Advanced`: a
/// chunk that neither halts nor hits a breakpoint always exhausts its
/// budget by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// Completed without interruption.
    Advanced,
    /// The machine halted.
    Halted,
    /// A resolved breakpoint address was reached.
    Breakpoint(u64),
    /// The instruction budget ran out first; the operation is resumable by
    /// calling the same method again.
    BudgetExhausted,
}

/// The loaded machine, its assembler, and the control-pane state layered on
/// top: breakpoints (by line, resolved to addresses), and whether a run or
/// chunked Step Over is in flight.
pub struct Control {
    mmix: MMix,
    assembler: MMixAssembler,
    filename: String,
    /// User-facing breakpoints, by source line. Survive a `reload` that
    /// re-assembles the same file, because a line number still means the
    /// same thing to the user even after addresses move.
    breakpoints: BTreeSet<usize>,
    /// `breakpoints` resolved to addresses against the current `assembler`.
    /// Kept in sync on load, reload, and every breakpoint toggle, so a run
    /// or step never re-walks the debug-info map per instruction.
    resolved_breakpoints: BTreeSet<u64>,
    /// Set while a chunked Run or a chunked Step Over is in flight; both
    /// share this flag, since only one can be in flight at a time and both
    /// are interrupted by Stop the same way.
    running: bool,
    /// Set only while a chunked Step Over is in flight: the call depth to
    /// return to before it's done. Distinguishes a Step Over continuation
    /// from a Run continuation when `running` is true, since both reuse the
    /// same chunk-yield loop.
    step_over_target_depth: Option<usize>,
    /// Set once the machine halts; cleared only by `reload`/`new` loading a
    /// fresh machine. There is no Reset control, so without this, Run,
    /// Step, or Step Over after a halt would execute whatever uninitialized
    /// memory sits past the halt instruction.
    halted: bool,
}

impl Control {
    /// Assemble `source`, load a fresh machine at the entry point, and stop
    /// -- nothing executed. No breakpoints yet; there is no prior state to
    /// preserve them from.
    pub fn new(source: &str, filename: &str) -> Result<Self, String> {
        let (mmix, assembler) = Self::assemble_and_load(source, filename)?;
        Ok(Self {
            mmix,
            assembler,
            filename: filename.to_string(),
            breakpoints: BTreeSet::new(),
            resolved_breakpoints: BTreeSet::new(),
            running: false,
            step_over_target_depth: None,
            halted: false,
        })
    }

    /// Re-assemble `source` and load a fresh machine -- what an edit does.
    /// Breakpoint line numbers survive; their resolved addresses are
    /// recomputed against the new assembly, since re-assembling moves
    /// addresses. On a parse error the previous machine and breakpoints are
    /// left untouched, same as today's error surfacing -- but a run or
    /// chunked Step Over in flight still stops, because the source shown
    /// alongside it is no longer the one that produced it, whether the
    /// re-assemble succeeds or fails.
    pub fn reload(&mut self, source: &str) -> Result<(), String> {
        self.running = false;
        self.step_over_target_depth = None;
        let (mmix, assembler) = Self::assemble_and_load(source, &self.filename)?;
        self.mmix = mmix;
        self.assembler = assembler;
        self.halted = false;
        self.resolve_breakpoints();
        Ok(())
    }

    fn assemble_and_load(source: &str, filename: &str) -> Result<(MMix, MMixAssembler), String> {
        let mut assembler = MMixAssembler::new(source, filename);
        assembler.parse()?;
        let mut mmix = MMix::new();
        write_image(&mut mmix, &assembler);
        mmix.set_pc(entry_point(&assembler));
        Ok((mmix, assembler))
    }

    fn resolve_breakpoints(&mut self) {
        self.resolved_breakpoints = self
            .breakpoints
            .iter()
            .filter_map(|&line| self.assembler.addr_for_line(&self.filename, line))
            .collect();
    }

    /// The loaded machine, for reading PC, registers, or memory.
    pub fn machine(&self) -> &MMix {
        &self.mmix
    }

    pub fn get_pc(&self) -> u64 {
        self.mmix.get_pc()
    }

    /// The current assembly's label table, for tagging addresses in the
    /// machine pane. Unlike the retired `Debugger::load`, which consumed
    /// the assembler once, `Control` keeps it alive for the session's
    /// life, re-set on every `reload`.
    pub fn labels(&self) -> &HashMap<String, u64> {
        &self.assembler.labels
    }

    /// Whether a chunked Run or chunked Step Over is in flight.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Whether the machine has halted since the last `reload`/`new`. Run,
    /// Step, and Step Over all no-op while this is set.
    pub fn is_halted(&self) -> bool {
        self.halted
    }

    pub fn call_depth(&self) -> usize {
        self.mmix.call_depth()
    }

    /// The 1-based source line the current PC maps to, or `None` for an
    /// address with no source mapping (compiler-generated code, or past the
    /// end of the program).
    pub fn current_line(&self) -> Option<usize> {
        self.assembler
            .source_loc(self.get_pc())
            .filter(|loc| loc.file == self.filename)
            .map(|loc| loc.line)
    }

    pub fn breakpoint_lines(&self) -> &BTreeSet<usize> {
        &self.breakpoints
    }

    /// Toggle a breakpoint on `line`. Returns `false`, a no-op, if `line`
    /// has no address in the current assembly (a blank line, a comment, or
    /// some directives), or if its address falls in the data segment (a
    /// `BYTE`/`OCTA`/etc. line) -- the program counter never reaches data,
    /// so a breakpoint there could never fire. Never silently sets a
    /// breakpoint that can't fire.
    pub fn toggle_breakpoint(&mut self, line: usize) -> bool {
        let Some(addr) = self.assembler.addr_for_line(&self.filename, line) else {
            return false;
        };
        if addr >= DATA_SEGMENT_START {
            return false;
        }
        if !self.breakpoints.remove(&line) {
            self.breakpoints.insert(line);
        }
        self.resolve_breakpoints();
        true
    }

    /// Begin a chunked Run. No-op once halted -- see `is_halted`.
    pub fn start_run(&mut self) {
        if self.halted {
            return;
        }
        self.running = true;
    }

    /// End a Run or chunked Step Over in flight, if any, leaving the
    /// machine where it stopped.
    pub fn stop(&mut self) {
        self.running = false;
        self.step_over_target_depth = None;
    }

    /// Execute exactly one instruction. Never checks breakpoints or a
    /// budget -- an explicit Step always executes, even onto a
    /// breakpointed line. No-op once halted.
    pub fn step(&mut self) -> StepOutcome {
        if self.halted {
            return StepOutcome::Halted;
        }
        if self.mmix.execute_instruction() {
            StepOutcome::Advanced
        } else {
            self.halted = true;
            StepOutcome::Halted
        }
    }

    /// `Debugger::do_next`'s rule, chunked: begin or continue a Step Over.
    ///
    /// A fresh call (no Step Over already in flight) executes the call
    /// instruction at the current PC -- never checking a breakpoint first,
    /// same as `step` -- and, if the call depth increased (`PUSHJ`/`PUSHGO`
    /// push a frame, `GO` does not), starts a chunked continuation back
    /// down to that depth. If the depth didn't increase, or it halted, this
    /// returns directly: there is nothing to continue.
    ///
    /// A continuation call (one already in flight) executes up to `budget`
    /// more instructions, stopping sooner on a halt, a resolved breakpoint,
    /// or the depth returning to the pre-call level -- the same shape
    /// `run_chunk` uses, so Step Over is stoppable and cannot block the
    /// event loop for a call that takes many instructions to return.
    ///
    /// Ends the run (`is_running()` becomes `false`) on every outcome
    /// except `BudgetExhausted`, which the caller is expected to yield on
    /// and call this again.
    pub fn step_over_chunk(&mut self, budget: usize) -> StepOutcome {
        if self.halted {
            return StepOutcome::Halted;
        }
        if self.step_over_target_depth.is_none() {
            let pre_call_depth = self.call_depth();
            if !self.mmix.execute_instruction() {
                self.halted = true;
                return StepOutcome::Halted;
            }
            if self.call_depth() <= pre_call_depth {
                return StepOutcome::Advanced;
            }
            self.running = true;
            self.step_over_target_depth = Some(pre_call_depth);
        }
        let target_depth = self
            .step_over_target_depth
            .expect("set above, or by a prior call that left a continuation in flight");

        let mut count = 0usize;
        while self.call_depth() > target_depth {
            if count >= budget {
                return StepOutcome::BudgetExhausted;
            }
            if self.resolved_breakpoints.contains(&self.get_pc()) {
                self.running = false;
                self.step_over_target_depth = None;
                return StepOutcome::Breakpoint(self.get_pc());
            }
            if !self.mmix.execute_instruction() {
                self.running = false;
                self.step_over_target_depth = None;
                self.halted = true;
                return StepOutcome::Halted;
            }
            count += 1;
        }
        self.running = false;
        self.step_over_target_depth = None;
        StepOutcome::Advanced
    }

    /// Execute up to `budget` instructions, stopping sooner on a halt or a
    /// resolved breakpoint -- the same shape as `Debugger::do_continue`:
    /// execute, then check. `run_bounded` cannot serve here even at the same
    /// budget: it hides every intermediate PC, so a breakpoint inside the
    /// chunk would only be caught at the chunk boundary, which is the exact
    /// granularity this loop exists to avoid.
    ///
    /// Never returns `StepOutcome::Advanced`: a chunk that neither halts nor
    /// hits a breakpoint always exhausts its budget. Ends the run
    /// (`is_running()` becomes `false`) on `Halted` or `Breakpoint`; leaves
    /// it running on `BudgetExhausted`, since the caller is expected to
    /// yield and call this again. No-op once halted.
    pub fn run_chunk(&mut self, budget: usize) -> StepOutcome {
        if self.halted {
            return StepOutcome::Halted;
        }
        let mut count = 0usize;
        loop {
            if count >= budget {
                return StepOutcome::BudgetExhausted;
            }
            if !self.mmix.execute_instruction() {
                self.running = false;
                self.halted = true;
                return StepOutcome::Halted;
            }
            count += 1;
            if self.resolved_breakpoints.contains(&self.get_pc()) {
                self.running = false;
                return StepOutcome::Breakpoint(self.get_pc());
            }
        }
    }

    /// Continue whichever chunked operation -- Run or Step Over -- is in
    /// flight, dispatching on `step_over_target_depth` so `main.rs`'s
    /// chunk-tick handler doesn't need to track which one it started.
    pub fn continue_chunk(&mut self, budget: usize) -> StepOutcome {
        if self.step_over_target_depth.is_some() {
            self.step_over_chunk(budget)
        } else {
            self.run_chunk(budget)
        }
    }
}

/// The single chunk-boundary yield point: hand control back to the browser
/// event loop, then call `callback`. A Stop request is checked only between
/// chunks (never inside one) because a chunk is bounded at `CHUNK_BUDGET` by
/// construction -- that bound is the whole reason a chunk yields at all, so
/// there is no second interruption mechanism to build here.
///
/// Returns the `Timeout` handle rather than leaking it via `forget()`: the
/// caller holds it (see `App::chunk_timeout`) so Stop, or a `reload` mid-run,
/// can cancel a pending tick by dropping it -- `Timeout`'s `Drop` calls
/// `clearTimeout` and frees the closure, `forget()` does neither.
pub fn yield_to_event_loop<F: FnOnce() + 'static>(callback: F) -> Timeout {
    Timeout::new(0, callback)
}

#[derive(Properties, PartialEq)]
pub struct ControlBarProps {
    pub running: bool,
    pub halted: bool,
    pub on_run: Callback<()>,
    pub on_step: Callback<()>,
    pub on_step_over: Callback<()>,
    pub on_stop: Callback<()>,
}

/// Run / Step / Step Over / Stop. Stop is enabled only while a run is in
/// flight; Step and Step Over only while one is not, and never once halted.
#[function_component(ControlBar)]
pub fn control_bar(props: &ControlBarProps) -> Html {
    let running = props.running;
    let halted = props.halted;
    let idle_only = running || halted;

    html! {
        <div class="controls">
            { control_button("Run", idle_only, props.on_run.clone()) }
            { control_button("Step", idle_only, props.on_step.clone()) }
            { control_button("Step Over", idle_only, props.on_step_over.clone()) }
            { control_button("Stop", !running, props.on_stop.clone()) }
            <span class="run-state">
                { run_state_label(running, halted) }
            </span>
        </div>
    }
}

/// One control-pane button: a plain `<button>` so every control is
/// keyboard-reachable without extra wiring.
fn control_button(label: &'static str, disabled: bool, on_click: Callback<()>) -> Html {
    let onclick = Callback::from(move |_| on_click.emit(()));
    html! {
        <button {disabled} {onclick}>{ label }</button>
    }
}

fn run_state_label(running: bool, halted: bool) -> &'static str {
    if running {
        "running"
    } else if halted {
        "halted"
    } else {
        "stopped"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A five-iteration countdown loop. Line 3 (`SUBI`, the loop body) is
    /// the natural breakpoint target: it runs on every iteration, so a
    /// breakpoint there fires on the very first pass.
    const LOOP_MMS: &str =
        "\tLOC\t#100\nMain\tSETL\t$1,5\nLoop\tSUBI\t$1,$1,1\n\tBNZ\t$1,Loop\n\tTRAP\t0,Halt,0\n";

    /// A call site (line 4, `PUSHJ`) into a one-instruction subroutine
    /// (line 7, `AddFunc`), returning at line 5.
    const CALL_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,40\n\tSETL\t$2,2\n\tPUSHJ\t$0,AddFunc\n\tSET\t$255,$0\n\tTRAP\t0,Halt,0\nAddFunc\tADDU\t$0,$0,$1\n\tPOP\t1,0\n";

    /// A call site (line 3, `PUSHJ`) into a two-instruction callee (lines 6
    /// and 7), long enough that a breakpoint on the callee's first
    /// instruction is distinguishable from stopping at the call's return.
    const CALL_WITH_BODY_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,1\n\tPUSHJ\t$0,Callee\n\tSET\t$255,$0\n\tTRAP\t0,Halt,0\nCallee\tADDU\t$0,$0,$1\n\tADDU\t$0,$0,$1\n\tPOP\t1,0\n";

    /// A call site (line 3, `PUSHJ`) into a subroutine (`Wait`) whose body
    /// (lines 6-8) loops five times before returning, long enough to
    /// exhaust a small chunk budget more than once.
    const CALL_WAIT_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,0\n\tPUSHJ\t$0,Wait\n\tTRAP\t0,Halt,0\nWait\tSETL\t$2,5\nWaitLoop\tADDU\t$1,$1,1\n\tSUBI\t$2,$2,1\n\tBNZ\t$2,WaitLoop\n\tPOP\t0,0\n";

    /// A non-halting counter loop, for chunk-exhaustion tests.
    const INFINITE_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,0\nLoop\tADDU\t$1,$1,1\n\tJMP\tLoop\n";

    /// A source that fails to parse: `BOGUS` is not a valid opcode.
    const INVALID_MMS: &str = "\tLOC\t#100\nMain\tBOGUS\t$1,1\n";

    /// The address `source`'s `line` assembles to, computed independently of
    /// any `Control` under test -- an oracle, not a readback.
    fn expect_addr(source: &str, filename: &str, line: usize) -> u64 {
        let mut assembler = MMixAssembler::new(source, filename);
        assembler.parse().expect("test program assembles");
        assembler
            .addr_for_line(filename, line)
            .unwrap_or_else(|| panic!("{filename}:{line} has no address"))
    }

    #[test]
    fn breakpoint_stops_the_chunk_at_the_right_instruction() {
        let mut control = Control::new(LOOP_MMS, "loop.mms").expect("assembles");
        assert!(control.toggle_breakpoint(3), "line 3 has an address");

        let expected_addr = expect_addr(LOOP_MMS, "loop.mms", 3);

        let outcome = control.run_chunk(1_000);

        // Deleting the breakpoint check from run_chunk would run this small
        // loop to completion instead, producing `Halted` here.
        assert_eq!(outcome, StepOutcome::Breakpoint(expected_addr));
        assert_eq!(control.get_pc(), expected_addr);
    }

    #[test]
    fn step_lands_inside_the_callee_step_over_lands_after_the_call() {
        // Two independent machines, both advanced to the PUSHJ call site
        // (line 4) by the same two plain steps from Main.
        let mut stepped = Control::new(CALL_MMS, "call.mms").expect("assembles");
        stepped.step();
        stepped.step();
        let mut stepped_over = Control::new(CALL_MMS, "call.mms").expect("assembles");
        stepped_over.step();
        stepped_over.step();

        let pre_call_depth = stepped.call_depth();
        assert_eq!(pre_call_depth, stepped_over.call_depth());

        let callee_addr = expect_addr(CALL_MMS, "call.mms", 7);
        let after_call_addr = expect_addr(CALL_MMS, "call.mms", 5);

        assert_eq!(stepped.step(), StepOutcome::Advanced);
        assert_eq!(
            stepped.get_pc(),
            callee_addr,
            "Step must land inside the callee"
        );

        // The callee is short enough to finish within one chunk budget, so
        // this completes in a single call -- see
        // `step_over_chunk_is_interruptible_by_stop` for a callee that
        // outlasts its budget and must be resumed.
        assert_eq!(
            stepped_over.step_over_chunk(CHUNK_BUDGET),
            StepOutcome::Advanced
        );
        assert_eq!(
            stepped_over.get_pc(),
            after_call_addr,
            "Step Over must land after the call"
        );
        // Reverting the depth rule to a plain single step would leave this
        // at the callee's address instead of back at the pre-call depth.
        assert_eq!(stepped_over.call_depth(), pre_call_depth);
        assert!(!stepped_over.is_running());
    }

    #[test]
    fn a_budget_exhausted_chunk_resumes_rather_than_restarts() {
        let mut control = Control::new(INFINITE_MMS, "loop.mms").expect("assembles");

        let first = control.run_chunk(3);
        assert_eq!(first, StepOutcome::BudgetExhausted);
        let pc_after_first = control.get_pc();
        let counter_after_first = control.machine().get_register(1);

        let second = control.run_chunk(3);
        assert_eq!(second, StepOutcome::BudgetExhausted);
        let pc_after_second = control.get_pc();
        let counter_after_second = control.machine().get_register(1);

        // A restart would reload at the entry point and repeat the same PC
        // and counter value; a resumed run keeps advancing through the loop
        // body instead.
        assert_ne!(pc_after_first, pc_after_second);
        assert!(
            counter_after_second > counter_after_first,
            "the loop counter must keep advancing across chunks, not reset"
        );
    }

    #[test]
    fn loading_a_program_executes_nothing() {
        let control = Control::new(CALL_MMS, "call.mms").expect("assembles");

        let entry = expect_addr(CALL_MMS, "call.mms", 2);
        assert_eq!(control.get_pc(), entry, "PC must sit at the entry point");

        // Main's first instruction is `SETL $1,40`; if it had run, $1 would
        // already be 40.
        assert_eq!(
            control.machine().get_register(1),
            0,
            "the entry instruction must not have executed yet"
        );
    }

    #[test]
    fn a_rejected_breakpoint_line_is_a_no_op() {
        let mut control = Control::new(LOOP_MMS, "loop.mms").expect("assembles");
        // Line 1 is a bare LOC directive: no address.
        assert!(!control.toggle_breakpoint(1));
        assert!(control.breakpoint_lines().is_empty());
    }

    #[test]
    fn a_breakpoint_on_a_data_segment_line_is_rejected() {
        // Reproduces against playmmix's own default program: `main.rs`'s
        // HELLO_WORLD_MMS, line 3, is `Text BYTE "Hello world!",'\n',0` --
        // a data line with a real, resolvable address the PC never reaches.
        let mut control = Control::new(crate::HELLO_WORLD_MMS, "hello.mms").expect("assembles");

        let addr = expect_addr(crate::HELLO_WORLD_MMS, "hello.mms", 3);
        assert!(
            addr >= DATA_SEGMENT_START,
            "line 3 must resolve into the data segment for this test to mean anything"
        );

        assert!(
            !control.toggle_breakpoint(3),
            "a data-segment line must never accept a breakpoint that can't fire"
        );
        assert!(control.breakpoint_lines().is_empty());
    }

    #[test]
    fn breakpoints_are_re_resolved_on_reload() {
        // Same source and line layout, loaded at a different address --
        // line 3 (the loop body) is still line 3, but its address moves.
        const SHIFTED_LOOP_MMS: &str = "\tLOC\t#200\nMain\tSETL\t$1,5\nLoop\tSUBI\t$1,$1,1\n\tBNZ\t$1,Loop\n\tTRAP\t0,Halt,0\n";

        let mut control = Control::new(LOOP_MMS, "loop.mms").expect("assembles");
        assert!(control.toggle_breakpoint(3));
        let stale_addr = expect_addr(LOOP_MMS, "loop.mms", 3);

        control.reload(SHIFTED_LOOP_MMS).expect("still assembles");
        let fresh_addr = expect_addr(SHIFTED_LOOP_MMS, "loop.mms", 3);
        assert_ne!(
            fresh_addr, stale_addr,
            "the edit must actually move the address for this test to mean anything"
        );

        assert_eq!(
            control
                .breakpoint_lines()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![3],
            "the breakpoint's line number survives the edit"
        );

        // A stale (pre-edit) resolved address is never reached in the
        // freshly loaded program's address space, so a run guided by it
        // would halt instead of hitting the breakpoint.
        let outcome = control.run_chunk(1_000);
        assert_eq!(outcome, StepOutcome::Breakpoint(fresh_addr));
        assert_eq!(control.get_pc(), fresh_addr);
    }

    #[test]
    fn reload_stops_a_run_in_flight_even_on_a_parse_error() {
        let mut control = Control::new(LOOP_MMS, "loop.mms").expect("assembles");
        control.start_run();
        assert!(control.is_running());

        let result = control.reload(INVALID_MMS);

        assert!(result.is_err(), "invalid source must surface as an error");
        assert!(
            !control.is_running(),
            "a parse error must still stop a run in flight, \
             not leave the stale machine executing unseen"
        );
    }

    #[test]
    fn reload_stops_a_chunked_step_over_in_flight_on_a_parse_error() {
        let mut control = Control::new(CALL_WAIT_MMS, "wait.mms").expect("assembles");
        control.step(); // land on the PUSHJ call site
        assert_eq!(
            control.step_over_chunk(1),
            StepOutcome::BudgetExhausted,
            "the wait loop must outlast a one-instruction chunk budget"
        );
        assert!(control.is_running());

        assert!(control.reload(INVALID_MMS).is_err());
        assert!(!control.is_running());

        // A stale continuation left in flight would route a later Run's
        // chunk tick through `step_over_chunk` (which stops as soon as the
        // call returns) instead of `run_chunk` (which runs straight through
        // to the halt). The failed reload leaves the old machine loaded, so
        // this can still run to completion.
        control.start_run();
        assert_eq!(control.continue_chunk(CHUNK_BUDGET), StepOutcome::Halted);
    }

    #[test]
    fn new_with_invalid_source_is_an_err() {
        assert!(Control::new(INVALID_MMS, "bad.mms").is_err());
    }

    #[test]
    fn reload_with_invalid_source_leaves_previous_machine_and_breakpoints_untouched() {
        let mut control = Control::new(LOOP_MMS, "loop.mms").expect("assembles");
        assert!(control.toggle_breakpoint(3));
        let pc_before = control.get_pc();

        let result = control.reload(INVALID_MMS);

        assert!(result.is_err());
        assert_eq!(
            control.get_pc(),
            pc_before,
            "the loaded machine is untouched"
        );
        assert_eq!(
            control
                .breakpoint_lines()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![3],
            "breakpoints are untouched"
        );
    }

    #[test]
    fn running_to_halt_then_stepping_or_running_again_is_a_no_op() {
        let mut control = Control::new(LOOP_MMS, "loop.mms").expect("assembles");
        control.start_run();
        let outcome = control.run_chunk(1_000);

        assert_eq!(outcome, StepOutcome::Halted);
        assert!(control.is_halted());
        assert!(
            !control.is_running(),
            "run_chunk must clear running on halt, not just leave it tracked elsewhere"
        );

        let pc_at_halt = control.get_pc();
        let register_at_halt = control.machine().get_register(1);

        // Every re-entry point must now be a no-op: PC and registers must
        // not move past the halt.
        assert_eq!(control.step(), StepOutcome::Halted);
        assert_eq!(control.get_pc(), pc_at_halt);
        assert_eq!(control.machine().get_register(1), register_at_halt);

        control.start_run();
        assert!(!control.is_running(), "start_run must no-op once halted");
        assert_eq!(control.run_chunk(1_000), StepOutcome::Halted);
        assert_eq!(control.get_pc(), pc_at_halt);

        assert_eq!(control.step_over_chunk(1_000), StepOutcome::Halted);
        assert_eq!(control.get_pc(), pc_at_halt);
        assert_eq!(control.machine().get_register(1), register_at_halt);
    }

    #[test]
    fn reload_clears_halted() {
        let mut control = Control::new(LOOP_MMS, "loop.mms").expect("assembles");
        control.start_run();
        control.run_chunk(1_000);
        assert!(control.is_halted());

        control.reload(LOOP_MMS).expect("still assembles");

        assert!(!control.is_halted(), "a fresh load must clear halted");
        assert_eq!(control.step(), StepOutcome::Advanced);
    }

    #[test]
    fn step_over_breakpoint_check_stops_mid_call_not_just_at_return() {
        let mut control = Control::new(CALL_WITH_BODY_MMS, "call.mms").expect("assembles");
        control.step(); // land on the PUSHJ call site (line 3)

        let callee_first_addr = expect_addr(CALL_WITH_BODY_MMS, "call.mms", 6);
        assert!(control.toggle_breakpoint(6));

        let outcome = control.step_over_chunk(CHUNK_BUDGET);

        // A step over that only checked for a breakpoint after the call
        // fully returned would run both callee instructions and land back
        // at line 4 (`Advanced`) instead of stopping here: the PC would
        // have advanced past `callee_first_addr` to the second `ADDU`, or
        // past the call entirely, rather than sitting on the breakpointed
        // instruction itself, not yet executed.
        assert_eq!(outcome, StepOutcome::Breakpoint(callee_first_addr));
        assert_eq!(control.get_pc(), callee_first_addr);
        assert!(!control.is_running());
    }

    #[test]
    fn step_over_chunk_is_interruptible_by_stop() {
        let mut control = Control::new(CALL_WAIT_MMS, "wait.mms").expect("assembles");
        control.step(); // land on the PUSHJ call site (line 3)

        assert_eq!(control.step_over_chunk(1), StepOutcome::BudgetExhausted);
        let counter_after_first = control.machine().get_register(1);
        assert!(control.is_running());

        assert_eq!(control.step_over_chunk(1), StepOutcome::BudgetExhausted);
        let counter_after_second = control.machine().get_register(1);
        assert!(
            counter_after_second > counter_after_first,
            "each chunk call must resume the call in progress, not restart it"
        );

        control.stop();
        assert!(!control.is_running());

        // Stopping mid-call must clear the pending continuation, not just
        // the running flag: a later Run must run straight through to the
        // halt via `run_chunk`. A stale continuation left in flight would
        // instead route it through `step_over_chunk`, which stops as soon
        // as the interrupted call returns -- well short of the halt.
        control.start_run();
        assert_eq!(control.continue_chunk(CHUNK_BUDGET), StepOutcome::Halted);
    }
}
