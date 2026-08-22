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

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use checksmix::{Host, MMix, MMixAssembler, entry_point, write_image};
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

/// Which stream a captured [`OutputSpan`] came from. `Diagnostic` is
/// checksmix's own operator-facing notices (an unhandled trap, a truncated
/// string, the HALT notice `handle_halt` always emits) -- a third class,
/// distinct from the program's own stdout/stderr, but appended to the same
/// buffer in arrival order so the output pane reads as one timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
    Diagnostic,
}

/// One captured write, in the order it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSpan {
    pub stream: OutputStream,
    pub text: String,
}

/// Shared handle to a program's captured output. `MMix::with_host` consumes
/// the host, so this `Rc` is the only way back to what it wrote -- held by
/// `Control`, cloned into the `Host` impl passed to `with_host`.
type OutputBuffer = Rc<RefCell<Vec<OutputSpan>>>;

/// Routes a loaded program's stdout (fd 1), stderr (fd 2), and diagnostics
/// into the shared [`OutputBuffer`] -- the seam that replaces `StdHost`
/// (whose `stdout()`/`stderr()` are a silent sink under
/// `wasm32-unknown-unknown`) with something the output pane can render.
struct CaptureHost {
    buffer: OutputBuffer,
}

impl Host for CaptureHost {
    fn write(&mut self, fd: u8, bytes: &[u8]) -> std::io::Result<()> {
        // checksmix confirms only fd 1 or 2 ever reach a `Host`; an
        // unrecognized fd (defensive only, never expected) is treated as
        // stdout rather than dropped, so no write silently vanishes.
        let stream = if fd == 2 {
            OutputStream::Stderr
        } else {
            OutputStream::Stdout
        };
        let text = String::from_utf8_lossy(bytes).into_owned();
        self.buffer.borrow_mut().push(OutputSpan { stream, text });
        Ok(())
    }

    fn now_micros(&mut self) -> u64 {
        // Unused by any example this prompt covers; matches checksmix's own
        // `Host` doctest.
        0
    }

    fn diagnostic(&mut self, msg: &str) {
        self.buffer.borrow_mut().push(OutputSpan {
            stream: OutputStream::Diagnostic,
            text: format!("{msg}\n"),
        });
    }
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
    /// Set the first time `step`, `run_chunk`, or `step_over_chunk` actually
    /// executes an instruction since the last `new`/`reload` -- including
    /// one that itself halts the machine. Distinguishes `paused` (something
    /// ran, then stopped) from `ready` (nothing has run yet) for the
    /// run-state label; never set by a halted no-op early return.
    has_advanced: bool,
    /// The current load's captured stdout/stderr/diagnostic output, in
    /// arrival order. Rebuilt by `assemble_and_load`, so both `new` and a
    /// successful `reload` start with an empty buffer automatically.
    output: OutputBuffer,
}

impl Control {
    /// Assemble `source`, load a fresh machine at the entry point, and stop
    /// -- nothing executed. No breakpoints yet; there is no prior state to
    /// preserve them from.
    pub fn new(source: &str, filename: &str) -> Result<Self, String> {
        let (mmix, assembler, output) = Self::assemble_and_load(source, filename)?;
        Ok(Self {
            mmix,
            assembler,
            filename: filename.to_string(),
            breakpoints: BTreeSet::new(),
            resolved_breakpoints: BTreeSet::new(),
            running: false,
            step_over_target_depth: None,
            halted: false,
            has_advanced: false,
            output,
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
        let (mmix, assembler, output) = Self::assemble_and_load(source, &self.filename)?;
        self.mmix = mmix;
        self.assembler = assembler;
        self.halted = false;
        self.has_advanced = false;
        self.output = output;
        self.resolve_breakpoints();
        Ok(())
    }

    fn assemble_and_load(
        source: &str,
        filename: &str,
    ) -> Result<(MMix, MMixAssembler, OutputBuffer), String> {
        let mut assembler = MMixAssembler::new(source, filename);
        assembler.parse()?;
        let output: OutputBuffer = Rc::new(RefCell::new(Vec::new()));
        let host = CaptureHost {
            buffer: output.clone(),
        };
        let mut mmix = MMix::with_host(host);
        write_image(&mut mmix, &assembler);
        mmix.set_pc(entry_point(&assembler));
        Ok((mmix, assembler, output))
    }

    fn resolve_breakpoints(&mut self) {
        self.resolved_breakpoints = self
            .breakpoints
            .iter()
            .filter_map(|&line| self.resolve_breakpoint_line(line))
            .collect();
    }

    /// Resolve `line` to an address a breakpoint can actually fire at, or
    /// `None` if it can't. Shared by `toggle_breakpoint` (deciding whether
    /// to accept a new breakpoint) and `resolve_breakpoints` (recomputing
    /// `resolved_breakpoints` from every stored line) so the two can never
    /// disagree about what a line resolves to.
    ///
    /// Tries `addr_for_line` first (a line that itself emits an
    /// instruction/directive), then falls back to treating the line's first
    /// whitespace-delimited token as a label -- checksmix's debug info only
    /// tags a line that emits code, so a label alone on its own line (legal
    /// MMIXAL) has no `addr_for_line` entry even though it resolves in
    /// `assembler.labels()`. No leading-whitespace precondition: checksmix's
    /// grammar has none, and the fallback only runs once `addr_for_line` has
    /// already failed for the line, so it can't collide with an ordinary
    /// instruction line's mnemonic. The label candidate must itself have a
    /// source mapping (`source_loc`), rejecting a trailing label past the
    /// last instruction, whose address is real but holds no instruction and
    /// so could never fire. Either path's address is rejected if it falls
    /// in the data segment, which the program counter never reaches.
    fn resolve_breakpoint_line(&self, line: usize) -> Option<u64> {
        let addr = self
            .assembler
            .addr_for_line(&self.filename, line)
            .or_else(|| {
                let text = self.assembler.source_text(&self.filename, line)?;
                let token = text.split_whitespace().next()?;
                let candidate = *self.assembler.labels.get(token)?;
                self.assembler.source_loc(candidate)?;
                Some(candidate)
            })?;
        if addr >= DATA_SEGMENT_START {
            None
        } else {
            Some(addr)
        }
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

    /// Whether `step`, `run_chunk`, or `step_over_chunk` has actually
    /// executed an instruction since the last `new`/successful `reload` --
    /// including one that itself halts the machine. False immediately after
    /// a fresh load; never set by a halted no-op early return.
    pub fn has_advanced(&self) -> bool {
        self.has_advanced
    }

    /// The address "where you are": once `halted`, `get_pc()` already points
    /// 4 bytes past the last real instruction (`handle_halt` advances the PC
    /// past the halting `TRAP` before returning), so this steps back to the
    /// instruction that actually ran. Otherwise identical to `get_pc()`.
    /// Both the editor's current-line lookup and the memory pane's
    /// current-row/current-instruction computation use this address, not
    /// the raw PC, so the marker lands on the halting instruction rather
    /// than past it.
    pub fn marker_pc(&self) -> u64 {
        if self.halted {
            self.get_pc().saturating_sub(4)
        } else {
            self.get_pc()
        }
    }

    /// The current load's captured stdout/stderr/diagnostic output, in
    /// arrival order.
    pub fn output(&self) -> Vec<OutputSpan> {
        self.output.borrow().clone()
    }

    /// The 1-based source line `marker_pc` maps to, or `None` for an address
    /// with no source mapping (compiler-generated code, or past the end of
    /// the program).
    pub fn current_line(&self) -> Option<usize> {
        self.assembler
            .source_loc(self.marker_pc())
            .filter(|loc| loc.file == self.filename)
            .map(|loc| loc.line)
    }

    pub fn breakpoint_lines(&self) -> &BTreeSet<usize> {
        &self.breakpoints
    }

    /// Toggle a breakpoint on `line`. Removing an already-set breakpoint
    /// always succeeds, even if `line` no longer resolves against the
    /// current assembly (source edited since it was set) -- otherwise a
    /// stale breakpoint could never be cleared. Setting a new one is gated
    /// on `resolve_breakpoint_line`: returns `false`, a no-op, if `line` has
    /// no address in the current assembly (a blank line, a comment, or some
    /// directives), or if its address falls in the data segment -- the
    /// program counter never reaches data, so a breakpoint there could
    /// never fire. Never silently sets a breakpoint that can't fire.
    pub fn toggle_breakpoint(&mut self, line: usize) -> bool {
        if self.breakpoints.remove(&line) {
            self.resolve_breakpoints();
            return true;
        }
        if self.resolve_breakpoint_line(line).is_none() {
            return false;
        }
        self.breakpoints.insert(line);
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

    /// Execute one source-level step. Never checks breakpoints or a budget
    /// -- an explicit Step always executes, even onto a breakpointed line.
    /// No-op once halted. See `step_instruction_group` for what "one
    /// source-level step" means when the current line expands to more than
    /// one physical instruction.
    pub fn step(&mut self) -> StepOutcome {
        if self.halted {
            return StepOutcome::Halted;
        }
        self.step_instruction_group()
    }

    /// Execute one physical instruction, then, only if both (a) the PC was
    /// already on a mapped source line before executing and (b) executing
    /// it didn't change `call_depth`, keep executing (bounded) until the PC
    /// reaches another mapped line -- hiding a pseudo-op's own internal
    /// words (`SETI`/`SET`/`LDA` compile to up to 4 physical words, tagged
    /// with a source line only on the first) so the current-line marker
    /// never lands mid-group.
    ///
    /// Condition (b) existing alone would also search from an already-
    /// unmapped PC (e.g. a second Step taken from inside the `debug`
    /// pseudo-op's generated subroutine), silently running further and
    /// executing side effects the user never asked for. Condition (a) rules
    /// that out: every Step taken from an already-unmapped PC is a plain
    /// single physical instruction, unconditionally -- if the PC just
    /// changed depth (a call) or was already unmapped, this stops
    /// immediately after the one instruction, mapped or not.
    ///
    /// The bound is 3 *additional* instructions: the largest group checksmix
    /// emits is 4 physical words total, and the head instruction just
    /// executed is one of those four, so at most 3 more are ever needed.
    ///
    /// Never checks a breakpoint mid-group, matching this contract's
    /// existing "never checks breakpoints" rule. Returns the `StepOutcome`
    /// of the last physical instruction actually executed. Callers must
    /// have already checked `self.halted`.
    fn step_instruction_group(&mut self) -> StepOutcome {
        let pc_was_mapped = self.assembler.source_loc(self.get_pc()).is_some();
        let pre_call_depth = self.call_depth();

        self.has_advanced = true;
        if !self.mmix.execute_instruction() {
            self.halted = true;
            return StepOutcome::Halted;
        }

        if pc_was_mapped && self.call_depth() == pre_call_depth {
            let mut budget = 3;
            while budget > 0 && self.assembler.source_loc(self.get_pc()).is_none() {
                if !self.mmix.execute_instruction() {
                    self.halted = true;
                    return StepOutcome::Halted;
                }
                budget -= 1;
            }
        }

        StepOutcome::Advanced
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
            // May execute up to 4 instructions, not necessarily 1 (see
            // `step_instruction_group`); the depth comparison below still
            // correctly reflects whatever actually happened, since it reads
            // `call_depth()` fresh rather than assuming a single call.
            if self.step_instruction_group() == StepOutcome::Halted {
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
            self.has_advanced = true;
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

/// Run/Step/Step Over/Stop/Reset's disabled state for a given
/// `(running, halted, has_error)` triple -- `docs/layout-spec.md`'s Run
/// lifecycle table, factored into one plain function so `ControlBar`'s body
/// states it once and a table-driven test can pin the whole table at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlEnablement {
    pub run_disabled: bool,
    pub step_disabled: bool,
    pub step_over_disabled: bool,
    pub stop_disabled: bool,
    pub reset_disabled: bool,
}

/// `has_error` forces every field `true`: an assembly error means there is
/// no valid program loaded to Run, Step, Step Over, Stop, or Reset, so every
/// control disables regardless of `running`/`halted`.
pub fn control_enablement(running: bool, halted: bool, has_error: bool) -> ControlEnablement {
    if has_error {
        return ControlEnablement {
            run_disabled: true,
            step_disabled: true,
            step_over_disabled: true,
            stop_disabled: true,
            reset_disabled: true,
        };
    }
    ControlEnablement {
        run_disabled: running,
        step_disabled: running || halted,
        step_over_disabled: running || halted,
        stop_disabled: !running,
        // Reset shares Run's running-only gate, so it is available in
        // every state Stop is not -- the "play again" control stays
        // reachable whether the machine is ready, paused, or halted.
        reset_disabled: running,
    }
}

#[derive(Properties, PartialEq)]
pub struct ControlBarProps {
    pub running: bool,
    pub halted: bool,
    /// Whether anything has executed since the last load -- distinguishes
    /// `paused` from `ready` in the run-state label.
    pub has_advanced: bool,
    /// Whether the currently displayed source has an assembly error --
    /// forces every control disabled (see `control_enablement`), since a
    /// broken program isn't the one that would actually run.
    pub has_error: bool,
    pub on_run: Callback<()>,
    pub on_step: Callback<()>,
    pub on_step_over: Callback<()>,
    pub on_stop: Callback<()>,
    pub on_reset: Callback<()>,
    /// A short (4-5 word) echo of the last action taken, rendered to the
    /// right of the run-state label -- `App::status_message` in `main.rs`.
    pub status: String,
}

/// Run / Step / Step Over / Stop / Reset, enabled per `control_enablement`.
#[function_component(ControlBar)]
pub fn control_bar(props: &ControlBarProps) -> Html {
    let running = props.running;
    let halted = props.halted;
    let enablement = control_enablement(running, halted, props.has_error);

    html! {
        <div class="controls">
            { control_button("Run", enablement.run_disabled, props.on_run.clone()) }
            { control_button("Step", enablement.step_disabled, props.on_step.clone()) }
            { control_button("Step Over", enablement.step_over_disabled, props.on_step_over.clone()) }
            { control_button("Stop", enablement.stop_disabled, props.on_stop.clone()) }
            { control_button("Reset", enablement.reset_disabled, props.on_reset.clone()) }
            <span class="run-state">
                { run_state_label(running, halted, props.has_advanced) }
            </span>
            <span class="status-message">{ &props.status }</span>
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

/// The run-state label: `running`/`halted` take precedence over whether
/// anything has executed; otherwise `paused` (something ran, then stopped)
/// or `stopped` (nothing has run since the last load) distinguish a fresh
/// load from a mid-program pause. `running && halted` cannot occur.
fn run_state_label(running: bool, halted: bool, has_advanced: bool) -> &'static str {
    if running {
        "running"
    } else if halted {
        "halted"
    } else if has_advanced {
        "paused"
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
        let mut control =
            Control::new(crate::examples::HELLO_WORLD_MMS, "hello.mms").expect("assembles");

        let addr = expect_addr(crate::examples::HELLO_WORLD_MMS, "hello.mms", 3);
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
    fn reload_with_invalid_source_leaves_has_advanced_and_output_untouched() {
        // Mirrors `reload_with_invalid_source_leaves_previous_machine_and_
        // breakpoints_untouched`, for the two fields that test doesn't cover:
        // a halted run has both `has_advanced` set and real output captured,
        // and a failed reload must leave both exactly as they were.
        let mut control =
            Control::new(crate::examples::HELLO_WORLD_MMS, "hello.mms").expect("assembles");
        let outcome = control.run_chunk(1_000_000);
        assert_eq!(outcome, StepOutcome::Halted, "fixture must reach a halt");
        let output_before = control.output();
        assert!(!output_before.is_empty(), "the halted run must have output");

        let result = control.reload(INVALID_MMS);

        assert!(result.is_err());
        assert!(
            control.has_advanced(),
            "has_advanced is untouched by a failed reload"
        );
        assert_eq!(
            control.output(),
            output_before,
            "captured output is untouched by a failed reload"
        );
    }

    #[test]
    fn marker_pc_steps_back_from_the_halted_pc_to_the_halting_instruction() {
        // Once halted, get_pc() sits 4 bytes past the halting TRAP
        // (handle_halt advances it before returning). marker_pc() must step
        // back to the instruction that actually ran, and current_line() must
        // resolve to that instruction's source line -- HELLO_WORLD_MMS's
        // `TRAP 0,Halt,0` is line 10.
        let mut control =
            Control::new(crate::examples::HELLO_WORLD_MMS, "hello.mms").expect("assembles");
        let outcome = control.run_chunk(1_000_000);
        assert_eq!(outcome, StepOutcome::Halted, "fixture must reach a halt");

        assert_eq!(
            control.marker_pc(),
            control.get_pc() - 4,
            "marker_pc must step back from the raw halted PC"
        );
        assert_eq!(
            control.current_line(),
            Some(10),
            "marker_pc must resolve to the halting TRAP's source line"
        );
    }

    #[test]
    fn marker_pc_equals_the_live_pc_while_not_halted() {
        // Not halted: marker_pc must equal get_pc() directly, with no
        // backward adjustment -- that only applies once actually halted.
        // Checked at the fresh-load PC and again after one step, so a
        // mutation that subtracts 4 unconditionally (not just when halted)
        // fails both.
        let mut control =
            Control::new(crate::examples::HELLO_WORLD_MMS, "hello.mms").expect("assembles");
        assert!(!control.is_halted());
        assert_eq!(control.marker_pc(), control.get_pc());

        assert_eq!(
            control.step(),
            StepOutcome::Advanced,
            "fixture's entry instruction must not halt"
        );
        assert!(!control.is_halted());
        assert_eq!(control.marker_pc(), control.get_pc());
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

    #[test]
    fn output_capture_includes_program_stdout_and_the_halt_diagnostic() {
        let mut control =
            Control::new(crate::examples::HELLO_WORLD_MMS, "hello.mms").expect("assembles");
        let outcome = control.run_chunk(1_000_000);
        assert_eq!(outcome, StepOutcome::Halted, "fixture must reach a halt");

        let output = control.output();
        let stdout_text: String = output
            .iter()
            .filter(|span| span.stream == OutputStream::Stdout)
            .map(|span| span.text.as_str())
            .collect();
        assert!(
            stdout_text.contains("Hello world!\n"),
            "the program's own Fputs output must be captured: {stdout_text:?}"
        );

        // `handle_halt` always calls `Host::diagnostic` on a halt, so the
        // buffer holds more than just the program's own output -- not
        // asserted as exact equality, since the diagnostic's PC value
        // varies by build.
        assert!(
            output
                .iter()
                .any(|span| span.stream == OutputStream::Diagnostic),
            "a halt must append a diagnostic line"
        );
    }

    #[test]
    fn has_advanced_tracks_whether_anything_has_executed() {
        let mut control = Control::new(LOOP_MMS, "loop.mms").expect("assembles");
        assert!(
            !control.has_advanced(),
            "a fresh load must not report having advanced"
        );

        control.step();
        assert!(control.has_advanced(), "one step must set has_advanced");

        control.reload(LOOP_MMS).expect("still assembles");
        assert!(
            !control.has_advanced(),
            "a successful reload must clear has_advanced"
        );
    }

    #[test]
    fn has_advanced_is_set_even_when_the_first_instruction_halts() {
        const HALTS_IMMEDIATELY_MMS: &str = "\tLOC\t#100\nMain\tTRAP\t0,Halt,0\n";
        let mut control = Control::new(HALTS_IMMEDIATELY_MMS, "halt.mms").expect("assembles");

        assert_eq!(control.step(), StepOutcome::Halted);
        assert!(
            control.has_advanced(),
            "the halting instruction still executed, so has_advanced must be true \
             even though the resulting state is halted, not paused"
        );

        // A second step is now a halted no-op and must not disturb
        // has_advanced (already true, but this pins the no-op path too).
        assert_eq!(control.step(), StepOutcome::Halted);
        assert!(control.has_advanced());
    }

    #[test]
    fn reset_via_reload_restores_fresh_load_values() {
        let mut control =
            Control::new(crate::examples::HELLO_WORLD_MMS, "hello.mms").expect("assembles");
        let fresh_pc = control.get_pc();

        let outcome = control.run_chunk(1_000_000);
        assert_eq!(outcome, StepOutcome::Halted, "fixture must reach a halt");
        assert!(control.is_halted());
        assert!(control.has_advanced());
        assert!(!control.output().is_empty());

        control
            .reload(crate::examples::HELLO_WORLD_MMS)
            .expect("still assembles");

        assert_eq!(control.get_pc(), fresh_pc, "PC returns to the entry point");
        assert!(!control.is_halted(), "halted clears on Reset");
        assert!(!control.has_advanced(), "has_advanced clears on Reset");
        assert!(control.output().is_empty(), "output clears on Reset");
    }

    #[test]
    fn control_enablement_matches_the_run_lifecycle_table() {
        // `docs/layout-spec.md`'s Run lifecycle table, restated as
        // (running, halted, has_error) -> disabled state for every control.
        let cases = [
            // (running, halted, has_error, run, step, step_over, stop, reset)
            (false, false, false, false, false, false, true, false), // ready
            (true, false, false, true, true, true, false, true),     // running
            // paused is (running=false, halted=false) after having advanced --
            // has_advanced doesn't affect enablement, only the label, so
            // "ready" and "paused" share one row here.
            (false, true, false, false, true, true, true, false), // halted
            // has_error forces every control disabled, regardless of what
            // running/halted would otherwise allow.
            (false, false, true, true, true, true, true, true), // ready + error
            (true, false, true, true, true, true, true, true),  // running + error
            (false, true, true, true, true, true, true, true),  // halted + error
        ];
        for (running, halted, has_error, run, step, step_over, stop, reset) in cases {
            let enablement = control_enablement(running, halted, has_error);
            assert_eq!(
                enablement.run_disabled, run,
                "Run disabled at running={running} halted={halted} has_error={has_error}"
            );
            assert_eq!(
                enablement.step_disabled, step,
                "Step disabled at running={running} halted={halted} has_error={has_error}"
            );
            assert_eq!(
                enablement.step_over_disabled, step_over,
                "Step Over disabled at running={running} halted={halted} has_error={has_error}"
            );
            assert_eq!(
                enablement.stop_disabled, stop,
                "Stop disabled at running={running} halted={halted} has_error={has_error}"
            );
            assert_eq!(
                enablement.reset_disabled, reset,
                "Reset disabled at running={running} halted={halted} has_error={has_error}"
            );
        }
    }

    #[test]
    fn run_state_label_matches_every_reachable_state() {
        // running && halted cannot occur.
        let cases = [
            (false, false, false, "stopped"),
            (false, false, true, "paused"),
            (true, false, false, "running"),
            (true, false, true, "running"),
            (false, true, false, "halted"),
            (false, true, true, "halted"),
        ];
        for (running, halted, has_advanced, expected) in cases {
            assert_eq!(
                run_state_label(running, halted, has_advanced),
                expected,
                "running={running} halted={halted} has_advanced={has_advanced}"
            );
        }
    }

    /// A five-iteration countdown loop identical to `LOOP_MMS`'s shape, but
    /// with the loop label on its own line -- legal MMIXAL, and the exact
    /// case `addr_for_line` can't resolve on its own.
    const LABEL_LINE_MMS: &str =
        "\tLOC\t#100\nMain\tSETL\t$1,5\nLoop\n\tSUBI\t$1,$1,1\n\tBNZ\t$1,Loop\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn breakpoint_on_a_standalone_label_line_resolves_via_the_label_fallback() {
        let mut control = Control::new(LABEL_LINE_MMS, "label.mms").expect("assembles");

        // Independent oracle: `Loop`'s address per the assembler's own label
        // table, read from a fresh assembler instance, not through `Control`.
        let mut oracle = MMixAssembler::new(LABEL_LINE_MMS, "label.mms");
        oracle.parse().expect("test program assembles");
        let label_addr = *oracle.labels.get("Loop").expect("Loop is a real label");
        assert!(
            oracle.addr_for_line("label.mms", 3).is_none(),
            "line 3 is the bare label; addr_for_line alone can't resolve it, \
             only the label fallback can"
        );

        assert!(
            control.toggle_breakpoint(3),
            "a standalone label line must be accepted, not silently ignored"
        );
        assert!(control.breakpoint_lines().contains(&3));

        // Exercise `resolve_breakpoints`'s output, not just
        // `toggle_breakpoint`'s return value: a revert that fixes only
        // `toggle_breakpoint` (leaving `resolve_breakpoints` ignorant of the
        // label fallback) would still pass the assertions above but fail
        // this one, since `run_chunk` checks `resolved_breakpoints`.
        let outcome = control.run_chunk(1_000);
        assert_eq!(outcome, StepOutcome::Breakpoint(label_addr));
        assert_eq!(control.get_pc(), label_addr);
    }

    #[test]
    fn a_trailing_label_past_the_last_instruction_is_rejected() {
        // `End` sits past the last real instruction: its address is real
        // (the label resolves) but holds no instruction, so a breakpoint
        // there could never fire.
        const TRAILING_LABEL_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,5\n\tTRAP\t0,Halt,0\nEnd\n";
        let mut control = Control::new(TRAILING_LABEL_MMS, "trailing.mms").expect("assembles");

        let mut oracle = MMixAssembler::new(TRAILING_LABEL_MMS, "trailing.mms");
        oracle.parse().expect("test program assembles");
        let end_addr = *oracle.labels.get("End").expect("End is a real label");
        assert!(
            oracle.source_loc(end_addr).is_none(),
            "End's address must hold no instruction for this test to mean anything"
        );

        assert!(
            !control.toggle_breakpoint(4),
            "a trailing label with no instruction at its address must be rejected"
        );
        assert!(control.breakpoint_lines().is_empty());
    }

    #[test]
    fn a_breakpoint_that_no_longer_resolves_can_still_be_cleared() {
        const SHORT_MMS: &str = "\tLOC\t#100\nMain\tTRAP\t0,Halt,0\n";

        let mut control = Control::new(LOOP_MMS, "loop.mms").expect("assembles");
        assert!(control.toggle_breakpoint(3), "line 3 has an address");

        control.reload(SHORT_MMS).expect("still assembles");
        assert!(
            control.breakpoint_lines().contains(&3),
            "the breakpoint's line number survives reload even though it no \
             longer resolves against the new source"
        );

        // Reverting the fix (gating removal on resolvability, same as
        // adding) would return `false` here instead, leaving line 3 stuck
        // forever.
        assert!(
            control.toggle_breakpoint(3),
            "clearing a breakpoint must never be gated on resolvability"
        );
        assert!(!control.breakpoint_lines().contains(&3));
    }

    #[test]
    fn step_crosses_a_multiword_pseudo_op_group_in_one_call() {
        // SETI compiles to exactly 4 physical words (SETH/SETMH/SETML/
        // SETL), tagged with a source line only on the first.
        const SETI_MMS: &str = "\tLOC\t#100\nMain\tSETI\t$1,40\n\tTRAP\t0,Halt,0\n";
        let mut control = Control::new(SETI_MMS, "seti.mms").expect("assembles");

        assert_eq!(control.step(), StepOutcome::Advanced);
        assert_eq!(
            control.machine().get_register(1),
            40,
            "the whole SETI group must have executed, not just its first word"
        );
        assert!(
            control.assembler.source_loc(control.marker_pc()).is_some(),
            "one step() call must land on a mapped address -- reverting the \
             fix would still be mid-group here (today it takes 4 calls)"
        );

        // The very next step() must be the TRAP: proof the first step()
        // consumed the entire 4-word group and nothing more.
        assert_eq!(control.step(), StepOutcome::Halted);
    }

    #[test]
    fn step_never_searches_past_a_call_into_unmapped_generated_code() {
        // Mirrors HELLO_WORLD_MMS's shape: a labeled `debug "..."` line as
        // the very first instruction, compiling to a PUSHJ into checksmix's
        // appended, entirely unmapped debug-print subroutine.
        let mut control =
            Control::new(crate::examples::HELLO_WORLD_MMS, "hello.mms").expect("assembles");
        let pre_call_depth = control.call_depth();

        // (1) The call itself: depth increases, landing on the callee's own
        // first instruction (SAVE), which has no source mapping at all.
        assert_eq!(control.step(), StepOutcome::Advanced);
        assert!(
            control.call_depth() > pre_call_depth,
            "PUSHJ must have pushed a call frame"
        );
        assert!(
            control.assembler.source_loc(control.marker_pc()).is_none(),
            "the debug subroutine's own SAVE instruction has no source mapping"
        );

        // (2) A second Step, taken from that already-unmapped PC: exactly
        // one more physical instruction, not a further search. Reverting
        // condition (b) (searching whenever depth is merely unchanged,
        // ignoring whether the pre-step PC was mapped) would instead run
        // several more instructions of the subroutine here, including the
        // diagnostic TRAP write -- a real side effect the user never asked
        // for.
        let pc_before_second = control.get_pc();
        let depth_before_second = control.call_depth();
        assert_eq!(control.step(), StepOutcome::Advanced);
        assert_eq!(
            control.call_depth(),
            depth_before_second,
            "SAVE must not itself change call depth, for this test to mean anything"
        );
        assert_eq!(
            control.get_pc(),
            pc_before_second + 4,
            "exactly one physical instruction must execute -- not a search"
        );
    }
}
