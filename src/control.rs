//! Execution model and control-pane UI: a run only happens when the user
//! asks for one, and can always be stopped.
//!
//! [`Control`] is the plain, host-testable core (`AGENTS.md`'s rule that
//! logic not needing browser APIs stays plain): it owns the loaded [`MMix`]
//! and its [`MMixAssembler`] across steps, the breakpoint line set, and
//! whether a run is in flight. No Yew, no browser API, no timers -- `cargo
//! test` drives it directly.
//!
//! [`Controls`] is the Yew-facing button bar. The chunked run loop itself
//! lives in `main.rs`'s `App`, which is what holds the machine across
//! renders (see that module) and therefore what decides when to call
//! [`Control::run_chunk`] and when to yield via [`yield_to_event_loop`].

use std::collections::BTreeSet;

use checksmix::{MMix, MMixAssembler, entry_point, write_image};
use gloo_timers::callback::Timeout;
use yew::prelude::*;

/// Instructions per chunk: the interrupt granularity for Run, since a run
/// can only be stopped at a chunk boundary. Not a throughput knob.
///
/// Measured under `wasm32-unknown-unknown` at `opt-level = 'z'` (the release
/// profile playmmix builds), running the same execute-and-check-breakpoint
/// loop `run_chunk` below uses, on the V8 engine (Node; the same wasm engine
/// Chrome embeds): 3,000,000 instructions ~= 250 ms, consistently across
/// repeated runs -- the ~250 ms worst-case Stop latency this is tuned for.
pub const CHUNK_BUDGET: usize = 3_000_000;

/// Why [`Control::step`], [`Control::step_over`], or [`Control::run_chunk`]
/// stopped. Close to [`checksmix::Stop`] but adds `Advanced`: a plain Step,
/// or a Step Over that completes its one instruction or call without
/// interruption, is neither a halt, a breakpoint, nor a budget exhaustion --
/// a distinction `run_chunk` never needs, since a chunk that does not halt
/// or hit a breakpoint always exhausts its budget by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// Completed without interruption: one instruction (`step`), or one
    /// instruction plus the rest of a call (`step_over`).
    Advanced,
    /// The machine halted.
    Halted,
    /// A resolved breakpoint address was reached.
    Breakpoint(u64),
    /// The instruction budget ran out first; the machine is resumable.
    BudgetExhausted,
}

/// The loaded machine, its assembler, and the control-pane state layered on
/// top: breakpoints (by line, resolved to addresses), and whether a run is
/// in flight.
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
    running: bool,
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
        })
    }

    /// Re-assemble `source` and load a fresh machine -- what an edit does.
    /// Breakpoint line numbers survive; their resolved addresses are
    /// recomputed against the new assembly, since re-assembling moves
    /// addresses. On a parse error the previous machine and breakpoints are
    /// left untouched, same as today's error surfacing.
    pub fn reload(&mut self, source: &str) -> Result<(), String> {
        let (mmix, assembler) = Self::assemble_and_load(source, &self.filename)?;
        self.mmix = mmix;
        self.assembler = assembler;
        self.running = false;
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

    pub fn is_running(&self) -> bool {
        self.running
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
    /// some directives) -- never silently sets a breakpoint that can't fire.
    pub fn toggle_breakpoint(&mut self, line: usize) -> bool {
        if self.assembler.addr_for_line(&self.filename, line).is_none() {
            return false;
        }
        if !self.breakpoints.remove(&line) {
            self.breakpoints.insert(line);
        }
        self.resolve_breakpoints();
        true
    }

    pub fn start_run(&mut self) {
        self.running = true;
    }

    /// End the run in flight, if any, leaving the machine where it stopped.
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Execute exactly one instruction. Never checks breakpoints or a
    /// budget -- an explicit Step always executes, even onto a
    /// breakpointed line.
    pub fn step(&mut self) -> StepOutcome {
        if self.mmix.execute_instruction() {
            StepOutcome::Advanced
        } else {
            StepOutcome::Halted
        }
    }

    /// `Debugger::do_next`'s rule: execute one instruction; if the call
    /// depth increased (`PUSHJ`/`PUSHGO` push a frame, `GO` does not), keep
    /// stepping until depth returns to the pre-call level, a breakpoint is
    /// reached, it halts, or `CHUNK_BUDGET` steps have run (a callee that
    /// never returns can't hang this). Otherwise stop after the one
    /// instruction, same as `step`.
    pub fn step_over(&mut self) -> StepOutcome {
        let pre_call_depth = self.call_depth();
        if !self.mmix.execute_instruction() {
            return StepOutcome::Halted;
        }
        let mut steps = 0usize;
        while self.call_depth() > pre_call_depth {
            if steps >= CHUNK_BUDGET {
                return StepOutcome::BudgetExhausted;
            }
            if self.resolved_breakpoints.contains(&self.get_pc()) {
                return StepOutcome::Breakpoint(self.get_pc());
            }
            if !self.mmix.execute_instruction() {
                return StepOutcome::Halted;
            }
            steps += 1;
        }
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
    /// yield and call this again.
    pub fn run_chunk(&mut self, budget: usize) -> StepOutcome {
        let mut count = 0usize;
        loop {
            if count >= budget {
                return StepOutcome::BudgetExhausted;
            }
            if !self.mmix.execute_instruction() {
                self.running = false;
                return StepOutcome::Halted;
            }
            count += 1;
            if self.resolved_breakpoints.contains(&self.get_pc()) {
                self.running = false;
                return StepOutcome::Breakpoint(self.get_pc());
            }
        }
    }
}

/// The single chunk-boundary yield point: hand control back to the browser
/// event loop, then call `callback`. A Stop request is checked only between
/// chunks (never inside one) because a chunk is bounded at `CHUNK_BUDGET` by
/// construction -- that bound is the whole reason a chunk yields at all, so
/// there is no second interruption mechanism to build here.
pub fn yield_to_event_loop<F: FnOnce() + 'static>(callback: F) {
    Timeout::new(0, callback).forget();
}

#[derive(Properties, PartialEq)]
pub struct ControlsProps {
    pub running: bool,
    pub on_run: Callback<()>,
    pub on_step: Callback<()>,
    pub on_step_over: Callback<()>,
    pub on_stop: Callback<()>,
}

/// Run / Step / Step Over / Stop. Stop is enabled only while a run is in
/// flight; Step and Step Over only while one is not. Plain `<button>`s so
/// every control is keyboard-reachable without extra wiring.
#[function_component(Controls)]
pub fn controls(props: &ControlsProps) -> Html {
    let on_run = props.on_run.clone();
    let on_step = props.on_step.clone();
    let on_step_over = props.on_step_over.clone();
    let on_stop = props.on_stop.clone();
    let running = props.running;

    html! {
        <div class="controls">
            <button
                class="control-run"
                disabled={running}
                onclick={Callback::from(move |_| on_run.emit(()))}
            >{ "Run" }</button>
            <button
                class="control-step"
                disabled={running}
                onclick={Callback::from(move |_| on_step.emit(()))}
            >{ "Step" }</button>
            <button
                class="control-step-over"
                disabled={running}
                onclick={Callback::from(move |_| on_step_over.emit(()))}
            >{ "Step Over" }</button>
            <button
                class="control-stop"
                disabled={!running}
                onclick={Callback::from(move |_| on_stop.emit(()))}
            >{ "Stop" }</button>
            <span class="run-state">
                { if running { "running" } else { "stopped" } }
            </span>
        </div>
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

    /// A non-halting counter loop, for chunk-exhaustion tests.
    const INFINITE_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,0\nLoop\tADDU\t$1,$1,1\n\tJMP\tLoop\n";

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

        assert_eq!(stepped_over.step_over(), StepOutcome::Advanced);
        assert_eq!(
            stepped_over.get_pc(),
            after_call_addr,
            "Step Over must land after the call"
        );
        // Reverting the depth rule to a plain single step would leave this
        // at the callee's address instead of back at the pre-call depth.
        assert_eq!(stepped_over.call_depth(), pre_call_depth);
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
}
