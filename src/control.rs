//! The execution engine: a run only happens when the user asks for one, and
//! can always be stopped.
//!
//! [`Control`] is the plain, host-testable core (`AGENTS.md`'s rule that
//! logic not needing browser APIs stays plain): it owns the loaded [`MMix`]
//! and its [`MMixAssembler`] across steps, the breakpoint line set, and
//! whether a run is in flight. No Yew, no browser API, no timers -- `cargo
//! test` drives it directly. The chunked run loop itself lives in
//! `main.rs`'s `App`, which is what holds the machine across renders (see
//! that module) and therefore what decides when to call
//! [`Control::continue_chunk`] or [`Control::next_chunk`] for a Continue's
//! or Next's own first, synchronous instruction, [`Control::resume_chunk`]
//! for every chunk tick that follows -- Run's included -- and when to
//! yield via [`yield_to_event_loop`].

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use checksmix::{MMix, MMixAssembler, SourceLoc, entry_point, start_program, write_image};
use gloo_timers::callback::Timeout;

use crate::output::{CaptureHost, OutputBuffer, OutputSpan};

/// The MMIX text/data segment boundary: the top three address bits select
/// the segment (0 text, 1 data, 2 pool, 3 stack), so any address at or past
/// this is data, not code. The program counter never enters data, so a
/// breakpoint resolved to such an address could never fire. checksmix uses
/// this same constant internally (`SEGMENT_BOUNDARY` in its `debugger.rs`,
/// `DATA_SEGMENT_START` in its `mmixal.rs`) but doesn't expose it; it's a
/// stable MMIX architectural boundary, safe to restate here.
const DATA_SEGMENT_START: u64 = 0x2000_0000_0000_0000;

/// Instructions per chunk: the interrupt granularity for Run, Continue, and
/// a chunked Next, since any of the three can only be interrupted at a
/// chunk boundary. Not a throughput knob.
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
/// case to ~245 ms (the actual worst-case Interrupt latency this budget is
/// tuned for) while dropping the tight-loop case to ~90 ms.
pub const CHUNK_BUDGET: usize = 1_000_000;

/// Why [`Control::step`], [`Control::next_chunk`], [`Control::run_chunk`],
/// or [`Control::continue_chunk`] stopped. Close to [`checksmix::Stop`] but
/// adds `Advanced`: the operation completed without being interrupted by a
/// halt, a breakpoint, or the budget running out -- a plain Step's one
/// instruction, or a Next reaching the pre-call depth (whether that took
/// one chunk or several). Neither `run_chunk` nor `continue_chunk` ever
/// returns `Advanced`: a chunk that neither halts nor hits a breakpoint
/// always exhausts its budget by construction.
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
/// chunked Next is in flight.
pub struct Control {
    mmix: MMix,
    assembler: MMixAssembler,
    filename: String,
    /// User-facing breakpoints, by source line. A line survives a `reload`
    /// that re-assembles the same file as long as it still resolves --
    /// a line number means the same thing to the user even after addresses
    /// move. A line the edit left unresolvable is dropped by
    /// `resolve_breakpoints`, so the gutter never marks a breakpoint that
    /// could not fire.
    breakpoints: BTreeSet<usize>,
    /// `breakpoints` resolved to addresses against the current `assembler`.
    /// Kept in sync on load, reload, and every breakpoint toggle, so a run
    /// or step never re-walks the debug-info map per instruction. One
    /// address per surviving line, but not necessarily a distinct one per
    /// line: a label-only line and the instruction line beneath it can
    /// both resolve to the same address via the label fallback (see
    /// `resolve_breakpoint_line`), so two entries in `breakpoints` may
    /// collapse to one entry here.
    resolved_breakpoints: BTreeSet<u64>,
    /// Set while a chunked Run, Continue, or Next is in flight; all three
    /// share this flag, since only one can be in flight at a time and all
    /// are interrupted the same way.
    running: bool,
    /// Set only while a chunked Next is in flight: the target it must
    /// return to before it's done -- see `next_reached` for the full
    /// stopping rule and why a depth-only field can't express it. Also
    /// distinguishes a Next continuation from a Run/Continue continuation
    /// when `running` is true, since all three reuse the same chunk-yield
    /// loop.
    next_target: Option<NextTarget>,
    /// Set once the machine halts; cleared only by `reload`/`new` loading a
    /// fresh machine. There is no way past a halt but Reset or Run's own
    /// restart, so without this, Step, Next, or Continue after a halt would
    /// execute whatever uninitialized memory sits past the halt
    /// instruction.
    halted: bool,
    /// Set the first time Run, Step, or Next is issued since the last
    /// `new`/`reload` -- including a Run that stops at a resolved
    /// breakpoint on the entry line before executing anything. Distinguishes
    /// `paused` (a started session that has since stopped) from `ready` (no session
    /// yet) for the run-state label, and gates Continue: gdb answers "The
    /// program is not being run" before a session starts. Cleared by
    /// `reload`, the one path every successful reload takes.
    session: bool,
    /// The current load's captured stdout/stderr/diagnostic output, in
    /// arrival order. Rebuilt by `assemble_and_load`, so both `new` and a
    /// successful `reload` start with an empty buffer automatically.
    output: OutputBuffer,
    /// Every text-segment address `write_image` actually wrote for this
    /// load -- computed once from `machine().loaded_extent()` on
    /// `new`/`reload` and cached, since that walks the whole image and
    /// `next_chunk`'s continuation loop checks it once per executed
    /// instruction. A membership test rather than a `[start, end]` bound:
    /// a program with more than one `LOC` in its text segment can leave a
    /// gap between two written regions, and a bound would read an address
    /// in that gap as loaded when it never was. Only running off the
    /// program's own end, jumping to any other address `write_image` never
    /// wrote -- a gap between `LOC`-separated regions, or anywhere else
    /// unwritten -- or landing on a PC at or above `Data_Segment`, leaves
    /// it. Empty for a program with nothing below `Data_Segment`;
    /// `left_loaded_image` then answers `true` unconditionally, so Next
    /// degrades to a plain Step rather than misreading nothing as
    /// everything.
    loaded_text_addresses: BTreeSet<u64>,
}

/// A chunked Next's target, captured once when it begins: the call depth
/// and source line to return to before it's done. `Control`'s `next_target`
/// field explains why the two travel together.
#[derive(Clone)]
struct NextTarget {
    depth: usize,
    origin: Option<SourceLoc>,
}

impl Control {
    /// Assemble `source`, load a fresh machine at the entry point, and stop
    /// -- nothing executed. No breakpoints yet; there is no prior state to
    /// preserve them from.
    pub fn new(source: &str, filename: &str) -> Result<Self, String> {
        let (mmix, assembler, output) = Self::assemble_and_load(source, filename)?;
        let loaded_text_addresses = Self::loaded_text_addresses(&mmix);
        Ok(Self {
            mmix,
            assembler,
            filename: filename.to_string(),
            breakpoints: BTreeSet::new(),
            resolved_breakpoints: BTreeSet::new(),
            running: false,
            next_target: None,
            halted: false,
            session: false,
            output,
            loaded_text_addresses,
        })
    }

    /// Re-assemble `source` and load a fresh machine -- what an edit does,
    /// what Reset does, and the start state Run always restarts through. A
    /// breakpoint line that still resolves survives with its address
    /// recomputed against the new assembly, since re-assembling moves
    /// addresses; one the edit left unresolvable is dropped entirely, so no
    /// gutter marker outlives the code it sat on. On a parse error the
    /// previous machine and breakpoints are left untouched (no pruning
    /// either), same as today's error surfacing -- but a run or chunked
    /// Next in flight still stops, because the source shown alongside it is
    /// no longer the one that produced it, whether the re-assemble succeeds
    /// or fails. The one path every successful reload takes, so it is also
    /// where a session ends.
    pub fn reload(&mut self, source: &str) -> Result<(), String> {
        self.running = false;
        self.next_target = None;
        let (mmix, assembler, output) = Self::assemble_and_load(source, &self.filename)?;
        self.loaded_text_addresses = Self::loaded_text_addresses(&mmix);
        self.mmix = mmix;
        self.assembler = assembler;
        self.halted = false;
        self.session = false;
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
        start_program(&mut mmix, entry_point(&assembler));
        Ok((mmix, assembler, output))
    }

    /// Every address `mmix`'s loaded text segment actually holds -- every
    /// address `write_image` wrote that isn't past `DATA_SEGMENT_START` --
    /// empty if it wrote no text address at all. `MMix::loaded_extent`
    /// yields addresses in ascending order and text addresses sort below
    /// every data address (segment 0 vs. segment 1+), so the text-segment
    /// prefix can be taken directly without visiting the data segment at
    /// all.
    fn loaded_text_addresses(mmix: &MMix) -> BTreeSet<u64> {
        mmix.loaded_extent()
            .map(|(addr, _)| addr)
            .take_while(|&addr| addr < DATA_SEGMENT_START)
            .collect()
    }

    /// Recompute `resolved_breakpoints` from `breakpoints` against the
    /// current assembly, dropping any line that no longer resolves from
    /// both sets at once -- otherwise a line whose code an edit deleted
    /// keeps its gutter marker while its breakpoint is permanently dead.
    ///
    /// Called from `reload` and from both of `toggle_breakpoint`'s arms.
    /// Only the `reload` call can ever prune: resolvability changes only
    /// when the assembly does, `toggle_breakpoint` refuses to store a line
    /// that doesn't resolve, and every reload prunes -- so by the time a
    /// toggle runs, every stored line already resolves.
    fn resolve_breakpoints(&mut self) {
        let resolved: Vec<(usize, u64)> = self
            .breakpoints
            .iter()
            .filter_map(|&line| Some((line, self.resolve_breakpoint_line(line)?)))
            .collect();
        self.breakpoints = resolved.iter().map(|&(line, _)| line).collect();
        self.resolved_breakpoints = resolved.into_iter().map(|(_, addr)| addr).collect();
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

    /// Whether the current assembly allocated any global register with
    /// `GREG`. `rG` alone cannot answer this: `GREG` allocates downward from
    /// `$254`, so 223 directives leave `rG` at exactly `32`, the same value
    /// an untouched machine has -- and `PUT`/`PUTI` can move `rG` off `32`
    /// with no `GREG` involved at all. `machine/registers.rs`'s global-range
    /// collapse needs both signals to tell a genuinely empty global range
    /// from either coincidence.
    pub fn has_greg_allocations(&self) -> bool {
        !self.assembler.greg_inits.is_empty()
    }

    /// Whether a chunked Run, Continue, or Next is in flight.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Whether the machine has halted since the last `reload`/`new`. Step,
    /// Next, and Continue all no-op while this is set; `start_run` does too,
    /// though Run's own restart never leaves this set by the time it calls
    /// `start_run`.
    pub fn is_halted(&self) -> bool {
        self.halted
    }

    pub fn call_depth(&self) -> usize {
        self.mmix.call_depth()
    }

    /// Whether a session has started: Run, Step, or Next has been issued
    /// since the last `new`/successful `reload` -- including a Run that
    /// stopped at a resolved breakpoint on the entry line before executing
    /// anything. Distinguishes `paused` (a started session that has since stopped)
    /// from `ready` (no session yet) for the run-state label, and gates
    /// Continue.
    pub fn session(&self) -> bool {
        self.session
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
    /// nothing was ever emitted at -- a gap between `LOC`-separated regions,
    /// or past the end of the program.
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
    /// always succeeds, ungated on whether `line` still resolves. Normal
    /// use can no longer reach that case -- `resolve_breakpoints` prunes an
    /// unresolvable line on every reload, so one can never be sitting there
    /// to clear -- but removal stays ungated on principle: nothing about
    /// forgetting a line should depend on the assembly. Setting a new one
    /// is gated on `resolve_breakpoint_line`: returns `false`, a no-op, if
    /// `line` has no address in the current assembly (a blank line, a
    /// comment, or some directives), or if its address falls in the data
    /// segment -- the program counter never reaches data, so a breakpoint
    /// there could never fire. Never silently sets a breakpoint that can't
    /// fire.
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

    /// Begin a chunked Run from the machine's current state, and start a
    /// session immediately -- not left to `run_chunk`'s own first call:
    /// `App::update` refreshes `shortcut_enablement` at the end of the very
    /// same `Msg::Run` that calls this, before the first `ChunkTick` ever
    /// reaches `run_chunk`, so an Interrupt landing in that window must
    /// already see a started session (`paused`, Continue live), not `ready`.
    /// `run_chunk` also sets this, redundantly for a call that went through
    /// here first, but not for a direct caller (a host test, or any future
    /// caller that skips `start_run`) -- see its own doc comment. No-op once
    /// halted -- see `is_halted`; Run's own restart always reloads first, so
    /// this guard never fires through the UI, only were `Control` called
    /// directly on an already-halted machine.
    pub fn start_run(&mut self) {
        if self.halted {
            return;
        }
        self.session = true;
        self.running = true;
    }

    /// End a Run, Continue, or Next in flight, if any, leaving the machine
    /// where it stopped -- the user's Interrupt, and `Msg::SourceChanged`'s
    /// own use when the edited source no longer matches what's running.
    pub fn end_in_flight(&mut self) {
        self.running = false;
        self.next_target = None;
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
    /// it didn't change `call_depth`, keep executing (bounded) while the PC
    /// stays mapped to that SAME statement -- hiding a pseudo-op's own
    /// internal words (`SETI`/`SET`/`LDA` compile to up to 4 physical
    /// words) so the current-line marker never lands mid-group.
    ///
    /// checksmix >=0.3.3 maps every address inside a multi-word statement's
    /// expansion to that statement's own line (previously only the first
    /// word was mapped, so "reached ANY mapped address" meant "reached the
    /// next statement" -- that's no longer true, since the second, third,
    /// and fourth words of the SAME group are mapped too, to the SAME
    /// line). The loop below compares the resolved `SourceLoc` itself
    /// (`PartialEq`), not just its presence, to tell "still inside the
    /// statement we started on" from "reached the next one."
    ///
    /// Condition (b) existing alone would also search from an already-
    /// unmapped PC -- e.g. a second Step taken after the program jumped
    /// into code it wrote itself at run time -- silently running further
    /// and executing side effects the user never asked for. Condition (a)
    /// rules that out: every Step taken from an already-unmapped PC is a
    /// plain single physical instruction, unconditionally -- if the PC just
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
        let head_loc = self.assembler.source_loc(self.get_pc()).cloned();
        let pre_call_depth = self.call_depth();

        self.session = true;
        if !self.mmix.execute_instruction() {
            self.halted = true;
            return StepOutcome::Halted;
        }

        if head_loc.is_some() && self.call_depth() == pre_call_depth {
            let mut budget = 3;
            while budget > 0 && self.assembler.source_loc(self.get_pc()) == head_loc.as_ref() {
                if !self.mmix.execute_instruction() {
                    self.halted = true;
                    return StepOutcome::Halted;
                }
                budget -= 1;
            }
        }

        StepOutcome::Advanced
    }

    /// Whether the PC now sits on a source line other than `origin` --
    /// `Debugger::reached_new_line`'s own rule, shared here with
    /// `next_chunk`. An address nothing was ever emitted at -- a gap
    /// between `LOC`-separated regions, or past the end of the program --
    /// answers false: it is inside no line, so it is not a new one.
    fn reached_new_line(&self, origin: Option<&SourceLoc>) -> bool {
        let Some(loc) = self.assembler.source_loc(self.get_pc()) else {
            return false;
        };
        match origin {
            Some(o) => loc != o,
            None => true,
        }
    }

    /// Whether the PC has left the loaded image: not a member of
    /// `loaded_text_addresses`, not what the source map alone would say.
    /// The source map resolves a data-segment address to a real line too --
    /// `HELLO_WORLD_MMS`'s `Text` label sits at `Data_Segment`, and
    /// `source_loc` maps it to line 3 -- so a PC that wandered into data
    /// would read as "reached a new line," not "left the image." checksmix's
    /// own `source_loc` also scans every instruction to find the item's
    /// size; `loaded_text_addresses` is a `BTreeSet`, one lookup per address
    /// `next_chunk`'s continuation loop checks. Only running off the
    /// program's own end, jumping to any other address `write_image` never
    /// wrote, or landing on a PC at or above `Data_Segment`, leaves it.
    fn left_loaded_image(&self) -> bool {
        !self.loaded_text_addresses.contains(&self.get_pc())
    }

    /// `Debugger::do_next`'s stopping rule, plus a case checksmix's own
    /// debugger never has to consider: the call depth is back at or below
    /// `target.depth` AND the PC has reached a source line other than
    /// `target.origin` -- depth alone would already read "reached" the
    /// instant a line that jumps back onto itself (`Spin JMP Spin`)
    /// finishes its own statement group, since a same-line jump never
    /// changes depth; a line-only rule would stop a self-recursive call
    /// before it actually returns, since the callee can share its caller's
    /// line -- OR the PC has left the loaded image entirely
    /// (`left_loaded_image`), since nothing sensible follows a PC that ran
    /// off the program's own end. Either half alone is not enough; see
    /// `left_loaded_image`'s own doc for why the source map can't take its
    /// place.
    fn next_reached(&self, target: &NextTarget) -> bool {
        self.left_loaded_image()
            || (self.call_depth() <= target.depth && self.reached_new_line(target.origin.as_ref()))
    }

    /// `Debugger::do_next`'s rule, chunked: begin or continue a Next.
    ///
    /// A fresh call (no Next already in flight) executes the statement at
    /// the current PC -- never checking a breakpoint first, same as
    /// `step` -- and, unless that alone already reached the target (see
    /// `next_reached`), starts a chunked continuation back to it. A call
    /// into a subroutine is exactly the case that alone-step misses: the
    /// callee's own return is many instructions away.
    ///
    /// A continuation call (one already in flight) executes up to `budget`
    /// more instructions, stopping sooner on a halt, a resolved breakpoint,
    /// or the target being reached -- the same shape `run_chunk` uses, so
    /// Next is stoppable and cannot block the event loop for a call that
    /// takes many instructions to return. Self-recursion whose callee
    /// entry shares its caller's line (`Loop PUSHJ $0,Loop`) runs on until a
    /// genuinely new line, which is `Debugger::do_next`'s own rule too --
    /// not a defect to "fix" here. A single-line self-loop with no call at
    /// all (`Loop JMP Loop`) never reaches a new line either, so it never
    /// terminates on its own -- stoppable only by Interrupt, same as Run on
    /// the same loop.
    ///
    /// Ends the run (`is_running()` becomes `false`) on every outcome
    /// except `BudgetExhausted`, which the caller is expected to yield on
    /// and call this again.
    pub fn next_chunk(&mut self, budget: usize) -> StepOutcome {
        if self.halted {
            return StepOutcome::Halted;
        }
        if self.next_target.is_none() {
            let pre_call_depth = self.call_depth();
            let origin = self.assembler.source_loc(self.get_pc()).cloned();
            // May execute up to 4 instructions, not necessarily 1 (see
            // `step_instruction_group`); the target check below still
            // correctly reflects whatever actually happened, since it reads
            // `call_depth()` and the PC's source location fresh rather than
            // assuming a single call.
            if self.step_instruction_group() == StepOutcome::Halted {
                return StepOutcome::Halted;
            }
            let target = NextTarget {
                depth: pre_call_depth,
                origin,
            };
            if self.next_reached(&target) {
                return StepOutcome::Advanced;
            }
            self.running = true;
            self.next_target = Some(target);
        }
        let target = self
            .next_target
            .clone()
            .expect("set above, or by a prior call that left a continuation in flight");

        let mut count = 0usize;
        while !self.next_reached(&target) {
            if count >= budget {
                return StepOutcome::BudgetExhausted;
            }
            if self.resolved_breakpoints.contains(&self.get_pc()) {
                self.running = false;
                self.next_target = None;
                return StepOutcome::Breakpoint(self.get_pc());
            }
            if !self.mmix.execute_instruction() {
                self.running = false;
                self.next_target = None;
                self.halted = true;
                return StepOutcome::Halted;
            }
            count += 1;
        }
        self.running = false;
        self.next_target = None;
        StepOutcome::Advanced
    }

    /// Execute up to `budget` instructions, stopping sooner on a halt or a
    /// resolved breakpoint -- the same shape as `Debugger::do_continue`:
    /// execute, then check. `run_bounded` cannot serve here even at the same
    /// budget: it hides every intermediate PC, so a breakpoint inside the
    /// chunk would only be caught at the chunk boundary, which is the exact
    /// granularity this loop exists to avoid.
    ///
    /// "Execute, then check" alone only ever inspects the PC a chunk
    /// *leaves*, never the one it *starts* on -- for every instruction but
    /// the first, that's the same address the previous instruction's own
    /// check already covered, so the two are equivalent. Not so for the
    /// very first instruction of a chunk: nothing has checked its starting
    /// PC yet, so a breakpoint sitting exactly there -- the program's entry
    /// point with no address-consuming label before it, or any other PC a
    /// Step/Reset/reload happened to land on -- would silently execute
    /// before this loop ever got a chance to see it. The entry check below
    /// closes that gap. It fires unconditionally, with no "I'm deliberately
    /// resuming past the breakpoint I last stopped at" exception: Run always
    /// restarts (see `docs/layout-spec.md`'s Run lifecycle), so the only way
    /// past a breakpoint is Continue, whose own unconditional first
    /// instruction (`continue_chunk`) never routes through this check at
    /// all.
    ///
    /// Never returns `StepOutcome::Advanced`: a chunk that neither halts nor
    /// hits a breakpoint always exhausts its budget. Ends the run
    /// (`is_running()` becomes `false`) on `Halted` or `Breakpoint`; leaves
    /// it running on `BudgetExhausted`, since the caller is expected to
    /// yield and call this again. No-op once halted. Also starts the session
    /// -- redundantly for a call `start_run` already started one for, but
    /// not for a direct caller that skips it, and this is the one call that
    /// must cover a Run stopped at the entry breakpoint before executing
    /// anything, whichever caller reaches it.
    pub fn run_chunk(&mut self, budget: usize) -> StepOutcome {
        if self.halted {
            return StepOutcome::Halted;
        }
        self.session = true;
        if self.resolved_breakpoints.contains(&self.get_pc()) {
            self.running = false;
            return StepOutcome::Breakpoint(self.get_pc());
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

    /// gdb's `continue`: execute the instruction at the PC unconditionally
    /// -- even a breakpointed one, so a Continue after a Step or Next lands
    /// on a breakpointed line still makes progress, unlike `run_chunk`'s own
    /// entry check -- then run up to `budget` further instructions exactly
    /// as `run_chunk` does, stopping sooner on a halt or a resolved
    /// breakpoint. No-op once halted; requires a started session, enforced
    /// by the caller's own enablement, not repeated here.
    pub fn continue_chunk(&mut self, budget: usize) -> StepOutcome {
        if self.halted {
            return StepOutcome::Halted;
        }
        self.running = true;
        if !self.mmix.execute_instruction() {
            self.running = false;
            self.halted = true;
            return StepOutcome::Halted;
        }
        if self.resolved_breakpoints.contains(&self.get_pc()) {
            self.running = false;
            return StepOutcome::Breakpoint(self.get_pc());
        }
        match budget.checked_sub(1) {
            Some(remaining) if remaining > 0 => self.run_chunk(remaining),
            _ => StepOutcome::BudgetExhausted,
        }
    }

    /// Resume whichever chunked operation -- Run, Continue, or Next -- is in
    /// flight, dispatching on `next_target` so `main.rs`'s chunk-tick
    /// handler doesn't need to track which one it started. A Continue's own
    /// chunk continuation resumes through `run_chunk`, not `continue_chunk`
    /// again: past its own first, unconditional instruction, a Continue in
    /// flight behaves exactly like a Run in flight -- the PC a `Budget
    /// Exhausted` tick leaves is never itself a resolved breakpoint, since
    /// the loop both methods share checks after every instruction, so
    /// `run_chunk`'s own entry check is always a no-op there.
    pub fn resume_chunk(&mut self, budget: usize) -> StepOutcome {
        if self.next_target.is_some() {
            self.next_chunk(budget)
        } else {
            self.run_chunk(budget)
        }
    }
}

/// The single chunk-boundary yield point: hand control back to the browser
/// event loop, then call `callback`. An Interrupt request is checked only
/// between chunks (never inside one) because a chunk is bounded at
/// `CHUNK_BUDGET` by construction -- that bound is the whole reason a chunk
/// yields at all, so there is no second interruption mechanism to build
/// here.
///
/// Returns the `Timeout` handle rather than leaking it via `forget()`: the
/// caller holds it (see `App::chunk_timeout`) so Interrupt, or a `reload`
/// mid-run, can cancel a pending tick by dropping it -- `Timeout`'s `Drop`
/// calls `clearTimeout` and frees the closure, `forget()` does neither.
pub fn yield_to_event_loop<F: FnOnce() + 'static>(callback: F) -> Timeout {
    Timeout::new(0, callback)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::control_bar::control_enablement;
    use crate::keys::keyboard_shortcut_for;
    use crate::output::OutputStream;

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

    /// A self-recursive call (line 2): the `PUSHJ`'s own target is the
    /// `PUSHJ` itself, so the callee's first instruction maps to the SAME
    /// source line the call started on.
    const SELF_RECURSIVE_CALL_MMS: &str = "\tLOC\t#100\nLoop\tPUSHJ\t$0,Loop\n\tPOP\t0,0\n";

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
    fn a_breakpoint_on_the_entry_point_stops_run_before_executing_it() {
        // Main's own instruction (line 2) IS the entry point: PC starts
        // there, before any chunk has executed a single instruction. The
        // mid-chunk check alone -- inspecting the PC a `mmix.execute_
        // instruction()` call *leaves* -- can never catch this, since
        // nothing has left this PC yet; only an entry check can.
        let mut control = Control::new(LOOP_MMS, "loop.mms").expect("assembles");
        assert!(control.toggle_breakpoint(2), "line 2 has an address");
        let expected_addr = expect_addr(LOOP_MMS, "loop.mms", 2);
        assert_eq!(control.get_pc(), expected_addr, "fixture assumption");

        let outcome = control.run_chunk(1_000);

        assert_eq!(outcome, StepOutcome::Breakpoint(expected_addr));
        // SETL $1,5 must not have executed: deleting the entry check would
        // run straight through it and stop only at the next mapped
        // breakpoint check, one instruction later than the user set.
        assert_eq!(
            control.machine().get_register(1),
            0,
            "the breakpointed instruction ran before this call ever inspected its own PC"
        );
    }

    #[test]
    fn step_lands_inside_the_callee_next_lands_after_the_call() {
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
        // `next_chunk_is_interruptible_by_interrupt` for a callee that
        // outlasts its budget and must be resumed.
        assert_eq!(stepped_over.next_chunk(CHUNK_BUDGET), StepOutcome::Advanced);
        assert_eq!(
            stepped_over.get_pc(),
            after_call_addr,
            "Next must land after the call"
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
    fn new_starts_a_program_the_way_mmixware_does() {
        // `start_program` sets the PC and `$255` together, MMIXware's own
        // start state: a program that never writes `$255` halts reporting
        // its own entry address as the exit code.
        let mut control =
            Control::new(crate::examples::DEFAULT_MMS, "default.mms").expect("assembles");
        assert_eq!(control.get_pc(), 0x100);
        assert_eq!(control.machine().get_register(255), 0x100);

        assert_eq!(control.run_chunk(1_000), StepOutcome::Halted);
        assert_eq!(control.machine().get_exit_code(), 256);
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
    fn reload_refreshes_the_loaded_text_address_cache() {
        // `loaded_text_addresses` is computed once on load and cached (see
        // its own doc for why). A reload that failed to recompute it would
        // leave Next's `left_loaded_image` check consulting the
        // *previous* load's addresses against the *new* machine's PC.
        const SHORT_MMS: &str = "\tLOC\t#100\nMain\tTRAP\t0,Halt,0\n";
        const LONGER_MMS: &str =
            "\tLOC\t#100\nMain\tSETL\t$1,1\n\tSETL\t$2,2\n\tSETL\t$3,3\n\tTRAP\t0,Halt,0\n";

        let mut control = Control::new(SHORT_MMS, "n.mms").expect("assembles");
        let addresses_before_reload = control.loaded_text_addresses.clone();

        control.reload(LONGER_MMS).expect("still assembles");

        assert_ne!(
            control.loaded_text_addresses, addresses_before_reload,
            "reload must recompute the cache for the newly loaded image, \
             not keep the previous load's addresses"
        );
        assert_eq!(
            control.loaded_text_addresses,
            Control::loaded_text_addresses(&control.mmix),
            "the cache must match what the current machine actually has \
             loaded"
        );
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
    fn reload_stops_a_chunked_next_in_flight_on_a_parse_error() {
        let mut control = Control::new(CALL_WAIT_MMS, "wait.mms").expect("assembles");
        control.step(); // land on the PUSHJ call site
        assert_eq!(
            control.next_chunk(1),
            StepOutcome::BudgetExhausted,
            "the wait loop must outlast a one-instruction chunk budget"
        );
        assert!(control.is_running());

        assert!(control.reload(INVALID_MMS).is_err());
        assert!(!control.is_running());

        // A stale continuation left in flight would route a later Run's
        // chunk tick (`resume_chunk`) through `next_chunk` (which stops as
        // soon as the call returns) instead of `run_chunk` (which runs
        // straight through to the halt). The failed reload leaves the old
        // machine loaded, so this can still run to completion.
        control.start_run();
        assert_eq!(control.resume_chunk(CHUNK_BUDGET), StepOutcome::Halted);
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
    fn reload_with_invalid_source_leaves_session_and_output_untouched() {
        // Mirrors `reload_with_invalid_source_leaves_previous_machine_and_
        // breakpoints_untouched`, for the two fields that test doesn't cover:
        // a halted run has both a started session and real output captured,
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
            control.session(),
            "the session is untouched by a failed reload"
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

        assert_eq!(control.next_chunk(1_000), StepOutcome::Halted);
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
    fn next_breakpoint_check_stops_mid_call_not_just_at_return() {
        let mut control = Control::new(CALL_WITH_BODY_MMS, "call.mms").expect("assembles");
        control.step(); // land on the PUSHJ call site (line 3)

        let callee_first_addr = expect_addr(CALL_WITH_BODY_MMS, "call.mms", 6);
        assert!(control.toggle_breakpoint(6));

        let outcome = control.next_chunk(CHUNK_BUDGET);

        // A Next that only checked for a breakpoint after the call
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
    fn next_chunk_is_interruptible_by_interrupt() {
        let mut control = Control::new(CALL_WAIT_MMS, "wait.mms").expect("assembles");
        control.step(); // land on the PUSHJ call site (line 3)

        assert_eq!(control.next_chunk(1), StepOutcome::BudgetExhausted);
        let counter_after_first = control.machine().get_register(1);
        assert!(control.is_running());

        assert_eq!(control.next_chunk(1), StepOutcome::BudgetExhausted);
        let counter_after_second = control.machine().get_register(1);
        assert!(
            counter_after_second > counter_after_first,
            "each chunk call must resume the call in progress, not restart it"
        );

        control.end_in_flight();
        assert!(!control.is_running());

        // Ending the in-flight call must clear the pending continuation,
        // not just the running flag: a later Run's chunk tick
        // (`resume_chunk`) must run straight through to the halt via
        // `run_chunk`. A stale continuation left in flight would instead
        // route it through `next_chunk`, which stops as soon as the
        // interrupted call returns -- well short of the halt.
        control.start_run();
        assert_eq!(control.resume_chunk(CHUNK_BUDGET), StepOutcome::Halted);
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
    fn session_tracks_whether_run_step_or_next_has_been_issued() {
        let mut control = Control::new(LOOP_MMS, "loop.mms").expect("assembles");
        assert!(!control.session(), "a fresh load must not report a session");

        control.step();
        assert!(control.session(), "one step must start a session");

        control.reload(LOOP_MMS).expect("still assembles");
        assert!(
            !control.session(),
            "a successful reload must end the session"
        );
    }

    #[test]
    fn session_starts_even_when_the_first_instruction_halts() {
        const HALTS_IMMEDIATELY_MMS: &str = "\tLOC\t#100\nMain\tTRAP\t0,Halt,0\n";
        let mut control = Control::new(HALTS_IMMEDIATELY_MMS, "halt.mms").expect("assembles");

        assert_eq!(control.step(), StepOutcome::Halted);
        assert!(
            control.session(),
            "the halting instruction was still issued via Step, so a session \
             must have started even though the resulting state is halted, \
             not paused"
        );

        // A second step is a halted no-op and must not disturb the session
        // (already started, but this pins the no-op path too).
        assert_eq!(control.step(), StepOutcome::Halted);
        assert!(control.session());
    }

    #[test]
    fn reset_via_reload_restores_fresh_load_values() {
        let mut control =
            Control::new(crate::examples::HELLO_WORLD_MMS, "hello.mms").expect("assembles");
        let fresh_pc = control.get_pc();

        let outcome = control.run_chunk(1_000_000);
        assert_eq!(outcome, StepOutcome::Halted, "fixture must reach a halt");
        assert!(control.is_halted());
        assert!(control.session());
        assert!(!control.output().is_empty());

        control
            .reload(crate::examples::HELLO_WORLD_MMS)
            .expect("still assembles");

        assert_eq!(control.get_pc(), fresh_pc, "PC returns to the entry point");
        assert!(!control.is_halted(), "halted clears on Reset");
        assert!(!control.session(), "the session ends on Reset");
        assert!(control.output().is_empty(), "output clears on Reset");
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
    fn a_breakpoint_whose_line_no_longer_resolves_is_pruned_on_reload() {
        // The loop body is gone: line 2 still holds an instruction, line 3
        // is now blank, and line 4 holds the halt.
        const EDITED_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,5\n\n\tTRAP\t0,Halt,0\n";

        let mut control = Control::new(LOOP_MMS, "loop.mms").expect("assembles");
        assert!(control.toggle_breakpoint(2), "line 2 has an address");
        assert!(control.toggle_breakpoint(3), "line 3 has an address");

        let mut oracle = MMixAssembler::new(EDITED_MMS, "loop.mms");
        oracle.parse().expect("test program assembles");
        assert!(
            oracle.addr_for_line("loop.mms", 3).is_none(),
            "the edit must actually leave line 3 unresolvable"
        );

        control.reload(EDITED_MMS).expect("still assembles");

        // Without pruning, line 3 keeps its gutter marker forever while
        // `resolved_breakpoints` silently drops it -- a dot that can never
        // fire again.
        assert_eq!(
            control
                .breakpoint_lines()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![2],
            "the unresolvable line must be pruned; the resolvable one must stay"
        );

        // Pruning runs inside `resolve_breakpoints`, which `toggle_breakpoint`
        // also calls -- a toggle must leave every other stored line alone.
        assert!(control.toggle_breakpoint(4), "line 4 has an address");
        assert_eq!(
            control
                .breakpoint_lines()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![2, 4]
        );
    }

    #[test]
    fn set_with_an_immediate_operand_assembles_and_runs() {
        // `SET $X,imm` is MMIXAL's alias for `SETL $X,imm` -- distinct from
        // `SET $X,$Y` (register-to-register, already covered by CALL_MMS
        // above). A checksmix grammar that only accepts the register form
        // rejects this at parse time.
        const SET_IMMEDIATE_MMS: &str = "\tLOC\t#100\nMain\tSET\t$1,40\n\tTRAP\t0,Halt,0\n";
        let mut control = Control::new(SET_IMMEDIATE_MMS, "set-imm.mms").expect("assembles");

        assert_eq!(control.run_chunk(1_000), StepOutcome::Halted);
        assert_eq!(control.machine().get_register(1), 40);
    }

    #[test]
    fn step_crosses_a_multiword_pseudo_op_group_in_one_call() {
        // SETI compiles to exactly 4 physical words (SETH/INCMH/INCML/
        // INCL), each tagged with the statement's own source line, not
        // just the first.
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
    fn step_never_searches_past_a_call_that_lands_on_its_own_source_line() {
        // A self-recursive call whose target is the `PUSHJ` instruction
        // itself exercises condition (b): the callee's first instruction
        // (the same `PUSHJ`) maps to the exact same, mapped source line the
        // call started on, so the inner search's line-equality check alone
        // would keep going. Only condition (b) -- the call changed depth --
        // stops it here.
        let mut control = Control::new(SELF_RECURSIVE_CALL_MMS, "loop.mms").expect("assembles");
        let pre_call_depth = control.call_depth();
        let call_addr = expect_addr(SELF_RECURSIVE_CALL_MMS, "loop.mms", 2);
        assert_eq!(control.get_pc(), call_addr, "fixture assumption");

        assert_eq!(control.step(), StepOutcome::Advanced);
        assert_eq!(
            control.get_pc(),
            call_addr,
            "the call's target is the PUSHJ instruction itself"
        );
        assert_eq!(
            control.call_depth(),
            pre_call_depth + 1,
            "exactly one PUSHJ must have executed -- reverting condition (b) \
             would let the group search chase this same, mapped source line \
             through further recursive calls instead of stopping at one"
        );
    }

    /// Writes three `ADDUI $3,$3,1` tetras, via `STTU`, to `#200` -- a text
    /// address none of this program's own `LOC` output ever touches -- then
    /// `GO`es there. `GO` doesn't push a register-stack frame, so it can't
    /// separate condition (a) from condition (b) the way a call would; only
    /// the landing PC's own absence from the source map does.
    const SELF_WRITTEN_CODE_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,#200\n\tSETI\t$2,#23030301\n\tSTTU\t$2,$1,0\n\tSTTU\t$2,$1,4\n\tSTTU\t$2,$1,8\n\tGO\t$5,$1,0\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn step_from_an_unmapped_pc_the_program_wrote_itself_never_searches_past_one_instruction() {
        // The Step that executes `GO` lands on `#200`, unmapped -- condition
        // (a) has nothing to do with that Step, since `head_loc` there is
        // still the mapped `GO` line itself. It is the NEXT Step, taken from
        // that already-unmapped PC, that exercises condition (a): `head_loc`
        // is now `None`, and depth alone (unchanged by the stored `ADDUI`s)
        // would otherwise pass condition (b) and enter the group search,
        // which would then keep matching `None == None` through the other
        // two stored instructions. Condition (a) stops the search before it
        // starts, so this Step is exactly one instruction.
        let mut control =
            Control::new(SELF_WRITTEN_CODE_MMS, "selfwritten.mms").expect("assembles");

        // Six source lines write the three tetras and GO to them.
        for _ in 0..6 {
            assert_eq!(control.step(), StepOutcome::Advanced, "setup must not halt");
        }
        let landing_pc = control.get_pc();
        assert_eq!(
            landing_pc, 0x200,
            "GO must land where the program wrote its own code"
        );
        assert!(
            control.assembler.source_loc(landing_pc).is_none(),
            "the landing address must have no source mapping -- it holds \
             only bytes the program stored at run time"
        );

        assert_eq!(
            control.step(),
            StepOutcome::Advanced,
            "executes one self-written ADDUI"
        );

        assert_eq!(
            control.get_pc() - landing_pc,
            4,
            "deleting condition (a) lets this step search into the other \
             two stored instructions instead of stopping after one"
        );
        assert_eq!(
            control.machine().get_register(3),
            1,
            "only the first stored ADDUI must have executed"
        );
    }

    #[test]
    fn next_on_a_debug_line_lands_on_the_next_source_line_in_one_call() {
        // `debug` compiles to a single, mapped `TRAP` at its own address --
        // Next must land on the line after it in this one call.
        let mut control =
            Control::new(crate::examples::HELLO_WORLD_MMS, "hello.mms").expect("assembles");
        let pre_call_depth = control.call_depth();
        let next_line_addr = expect_addr(crate::examples::HELLO_WORLD_MMS, "hello.mms", 8);

        let outcome = control.next_chunk(CHUNK_BUDGET);

        assert_eq!(outcome, StepOutcome::Advanced);
        assert_eq!(
            control.call_depth(),
            pre_call_depth,
            "the debug TRAP never pushes a call frame, so depth must be unchanged"
        );
        assert_eq!(
            control.get_pc(),
            next_line_addr,
            "Next must land on the line after the debug directive"
        );
        assert!(
            control.assembler.source_loc(control.marker_pc()).is_some(),
            "the landing PC must resolve to a real source line"
        );
        assert!(!control.is_running());
    }

    #[test]
    fn next_stops_at_the_end_of_the_image_instead_of_halting() {
        // This program has no halting TRAP: once its two lines are done, the
        // PC runs into memory write_image never wrote. An unmapped PC alone
        // reads as "not a new line" (`reached_new_line`), so without a check
        // against the loaded image, the continuation loop would keep going
        // through that unwritten memory until it happened to decode into
        // something checksmix treats as an unhandled trap -- latching
        // `halted` for a Next the user never asked to run that far. A
        // plain Step here does not have this problem: `step_instruction_
        // group`'s own group search already stops the instant the PC leaves
        // the mapped line, so this pins Next to that same landing spot.
        const RUNS_OFF_THE_END_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,1\n\tSETL\t$2,2\n";

        let mut stepped = Control::new(RUNS_OFF_THE_END_MMS, "n.mms").expect("assembles");
        stepped.step(); // land on line 3
        assert_eq!(
            stepped.step(),
            StepOutcome::Advanced,
            "a plain Step must not halt here -- fixture assumption"
        );
        let pc_after_plain_steps = stepped.get_pc();

        let mut control = Control::new(RUNS_OFF_THE_END_MMS, "n.mms").expect("assembles");
        control.step(); // land on line 3

        let outcome = control.next_chunk(CHUNK_BUDGET);

        assert_eq!(
            outcome,
            StepOutcome::Advanced,
            "leaving the loaded image must stop Next cleanly, not run it \
             into an unintended halt"
        );
        assert!(
            !control.is_halted(),
            "Control::halted's own doc: this must never latch from running \
             off the end of the image, or Run/Step/Next stay disabled \
             until Reset for a halt the program never actually reached"
        );
        assert!(
            control.output().is_empty(),
            "no diagnostic or write must come from unmapped memory"
        );
        assert_eq!(
            control.get_pc(),
            pc_after_plain_steps,
            "Next must land exactly where a plain Step would"
        );
    }

    #[test]
    fn next_stops_at_a_gap_between_loc_regions_instead_of_running_into_it() {
        // Two `LOC` directives in the text segment leave a gap between them
        // that `write_image` never wrote -- ordinary MMIXAL, not a
        // pathology. A `[start, end]` bound over the text segment reads an
        // address in that gap as loaded when it never was, so the
        // continuation loop would keep executing through it, decoding
        // unwritten memory until it happens to read as `TRAP 0,Halt,0` --
        // latching `halted` for a Next the user never asked to run
        // that far, same class of defect as running off the program's own
        // end. `loaded_text_addresses` is a membership set, not a bound, so
        // it tells a real gap apart from loaded memory.
        const GAP_BETWEEN_LOC_REGIONS_MMS: &str =
            "\tLOC\t#100\nMain\tSETL\t$1,1\n\tLOC\t#108\n\tSETL\t$2,2\n\tTRAP\t0,Halt,0\n";

        let mut stepped = Control::new(GAP_BETWEEN_LOC_REGIONS_MMS, "n.mms").expect("assembles");
        assert_eq!(
            stepped.step(),
            StepOutcome::Advanced,
            "a plain Step must not halt here -- fixture assumption"
        );
        let pc_after_plain_step = stepped.get_pc();

        let mut control = Control::new(GAP_BETWEEN_LOC_REGIONS_MMS, "n.mms").expect("assembles");

        let outcome = control.next_chunk(CHUNK_BUDGET);

        assert_eq!(
            outcome,
            StepOutcome::Advanced,
            "landing in the gap must stop Next cleanly, not run it \
             into an unintended halt"
        );
        assert!(
            !control.is_halted(),
            "Control::halted's own doc: this must never latch from running \
             into a gap between loaded regions, or Run/Step/Next stay \
             disabled until Reset for a halt the program never actually \
             reached"
        );
        assert!(
            control.output().is_empty(),
            "no diagnostic or write must come from unmapped memory"
        );
        assert_eq!(
            control.get_pc(),
            pc_after_plain_step,
            "Next must land exactly where a plain Step would"
        );
    }

    #[test]
    fn next_on_a_debug_line_that_is_the_programs_last_statement_prints_once_then_stops_at_the_image_edge()
     {
        // When `debug` is the program's last statement, nothing follows its
        // own `TRAP` in the loaded image: the PC lands past everything
        // `write_image` wrote, so `left_loaded_image` fires and Next stops
        // there with `Advanced`, not `Halted` -- the same image-edge rule
        // `next_stops_at_the_end_of_the_image_instead_of_halting` pins for a
        // program with no `debug` at all. This pins that the debug print
        // still runs exactly once in this one call, and that stdout stays
        // at one copy of "bye\n" -- not two -- across both this stop and a
        // further Next, which then does run into truly unwritten memory and
        // halts there.
        const DEBUG_LAST_MMS: &str = "\tLOC\t#100\nMain\tdebug \"bye\"\n";
        let mut control = Control::new(DEBUG_LAST_MMS, "x.mms").expect("assembles");
        let stdout_text = |control: &Control| -> String {
            control
                .output()
                .iter()
                .filter(|span| span.stream == OutputStream::Stdout)
                .map(|span| span.text.as_str())
                .collect()
        };

        let outcome = control.next_chunk(CHUNK_BUDGET);

        assert_eq!(outcome, StepOutcome::Advanced);
        assert!(!control.is_halted());
        assert_eq!(
            stdout_text(&control),
            "bye\n",
            "the debug print must have run exactly once in this one call"
        );

        // Nothing follows the debug line -- a further Next runs into memory
        // write_image never wrote, decodes it as TRAP 0,Halt,0, and halts.
        let second = control.next_chunk(CHUNK_BUDGET);

        assert_eq!(second, StepOutcome::Halted);
        assert!(control.is_halted());
        assert_eq!(
            stdout_text(&control),
            "bye\n",
            "a further Next must not print bye a second time"
        );
    }

    #[test]
    fn next_on_a_line_that_jumps_to_itself_keeps_running_until_interrupted() {
        // `next_reached`'s line check must actually gate Next: a line that
        // jumps to itself (`Spin JMP Spin`) never reaches a new line, and
        // its call depth never changes either, so depth alone would already
        // read "reached" the instant its own statement group finishes. With
        // the line check, Next can't tell that apart from real progress, so
        // it keeps running -- gdb's `next` on a one-line loop, stoppable
        // only by Interrupt.
        const SPIN_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,1\nSpin\tJMP\tSpin\n";
        let mut control = Control::new(SPIN_MMS, "spin.mms").expect("assembles");
        control.step(); // land on Spin's line
        let spin_addr = expect_addr(SPIN_MMS, "spin.mms", 3);
        assert_eq!(control.get_pc(), spin_addr, "fixture assumption");
        assert_eq!(spin_addr, 0x104);

        let outcome = control.next_chunk(10);

        assert_eq!(
            outcome,
            StepOutcome::BudgetExhausted,
            "deleting the line check would report Advanced here instead, \
             since depth alone is already satisfied on a line with no call"
        );
        assert!(control.is_running());
        assert_eq!(
            control.get_pc(),
            spin_addr,
            "the loop never leaves its own line"
        );

        // Nothing about the loop itself ever stops it -- only Interrupt
        // (`end_in_flight`) does.
        control.end_in_flight();
        assert!(
            !control.is_running(),
            "Interrupt must stop a Next that never reaches a new line"
        );
    }

    #[test]
    fn debug_prints_after_put_rg_255_makes_dollar_254_local() {
        // Pins that `debug` prints cleanly once `PUT rG,255` has moved
        // `$254` into the local range: its `TRAP 0,Debug,K` touches no
        // register, so nothing about `rG` reaches it. At 0.3.10 the same
        // sequence halted instead -- the old stub's own `SAVE $254,0`
        // requires `X` global (`X >= rG`), which `$254` no longer was.
        // `next_chunk` stops on the line after `debug`, before its own
        // trailing `TRAP 0,Halt,0` ever runs, so "no diagnostic at all"
        // unambiguously means this printed instead of halting.
        const PUT_RG_THEN_DEBUG_MMS: &str =
            "\tLOC\t#100\nMain\tPUT\trG,255\n\tdebug \"hi\"\n\tTRAP\t0,Halt,0\n";
        let mut control = Control::new(PUT_RG_THEN_DEBUG_MMS, "rg.mms").expect("assembles");

        assert_eq!(control.step(), StepOutcome::Advanced, "PUT rG,255");
        assert_eq!(control.next_chunk(CHUNK_BUDGET), StepOutcome::Advanced);
        assert!(!control.is_halted());

        let output = control.output();
        let stdout_text: String = output
            .iter()
            .filter(|span| span.stream == OutputStream::Stdout)
            .map(|span| span.text.as_str())
            .collect();
        assert_eq!(stdout_text, "hi\n", "debug must print after PUT rG,255");
        assert!(
            !output
                .iter()
                .any(|span| span.stream == OutputStream::Diagnostic),
            "no diagnostic must appear -- the old stub's SAVE $254,0 would \
             have halted here instead of printing"
        );
    }

    /// A straight-line program -- no loop, no call -- long enough that
    /// Step or Next can land on line 3 without reaching the halt.
    const STRAIGHT_LINE_MMS: &str =
        "\tLOC\t#100\nMain\tSETL\t$1,1\n\tSETL\t$2,2\n\tSETL\t$3,3\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn continue_needs_a_started_session() {
        let mut control = Control::new(STRAIGHT_LINE_MMS, "continue.mms").expect("assembles");

        // A fresh load: no session yet.
        assert!(!control.session());
        let ready = control_enablement(
            control.is_running(),
            control.is_halted(),
            control.session(),
            false,
        );
        assert!(
            ready.continue_disabled,
            "Continue must be disabled on a fresh load"
        );
        assert_eq!(
            keyboard_shortcut_for("c", false, false, false, ready),
            None,
            "c must map nothing on a fresh load"
        );

        // Reset (`Control::reload`): the session ends the same way.
        control.reload(STRAIGHT_LINE_MMS).expect("still assembles");
        assert!(!control.session());
        let after_reset = control_enablement(
            control.is_running(),
            control.is_halted(),
            control.session(),
            false,
        );
        assert!(
            after_reset.continue_disabled,
            "Continue must be disabled after Reset"
        );

        // A breakpoint on the entry line: Run stops there before executing
        // anything, but the session has still started.
        assert!(control.toggle_breakpoint(2), "line 2 has an address");
        let entry_addr = expect_addr(STRAIGHT_LINE_MMS, "continue.mms", 2);
        assert_eq!(control.get_pc(), entry_addr, "fixture assumption");
        assert_eq!(
            control.run_chunk(CHUNK_BUDGET),
            StepOutcome::Breakpoint(entry_addr)
        );
        assert_eq!(
            control.machine().get_register(1),
            0,
            "the breakpointed instruction must not have executed"
        );
        assert!(
            control.session(),
            "a Run stopped at the entry breakpoint must still start a session -- \
             the run-state label reads `paused` here, per `run_state_label`"
        );
        let paused = control_enablement(
            control.is_running(),
            control.is_halted(),
            control.session(),
            false,
        );
        assert!(
            !paused.continue_disabled,
            "Continue must be enabled once paused, even at the entry breakpoint"
        );

        // Continue is the only way past that breakpoint: it executes past
        // it, straight through to the halt (no further breakpoints ahead).
        assert_eq!(control.continue_chunk(CHUNK_BUDGET), StepOutcome::Halted);
        assert!(control.is_halted());

        // Run restarts into the same entry breakpoint again.
        control.reload(STRAIGHT_LINE_MMS).expect("still assembles");
        assert_eq!(
            control.run_chunk(CHUNK_BUDGET),
            StepOutcome::Breakpoint(entry_addr)
        );
    }

    #[test]
    fn continue_always_moves_past_a_breakpoint_step_or_next_landed_on() {
        let line3_addr = expect_addr(STRAIGHT_LINE_MMS, "continue-always-moves.mms", 3);

        // Step lands on the breakpointed line.
        let mut stepped =
            Control::new(STRAIGHT_LINE_MMS, "continue-always-moves.mms").expect("assembles");
        assert!(stepped.toggle_breakpoint(3));
        assert_eq!(stepped.step(), StepOutcome::Advanced);
        assert_eq!(
            stepped.get_pc(),
            line3_addr,
            "fixture assumption: Step must land on the breakpointed line"
        );

        // Deleting the fix (reverting to an entry check like `run_chunk`'s)
        // would immediately re-report this same breakpoint without
        // executing it, leaving the PC exactly where it started.
        stepped.continue_chunk(CHUNK_BUDGET);
        assert_ne!(
            stepped.get_pc(),
            line3_addr,
            "Continue must execute the breakpointed instruction and move off it"
        );
        assert_eq!(
            stepped.machine().get_register(2),
            2,
            "line 3's SETL must have executed"
        );

        // Next lands on the same breakpointed line (no call in this
        // fixture, so Next behaves as one physical step here).
        let mut stepped_over =
            Control::new(STRAIGHT_LINE_MMS, "continue-always-moves.mms").expect("assembles");
        assert!(stepped_over.toggle_breakpoint(3));
        assert_eq!(stepped_over.next_chunk(CHUNK_BUDGET), StepOutcome::Advanced);
        assert_eq!(
            stepped_over.get_pc(),
            line3_addr,
            "fixture assumption: Next must land on the breakpointed line"
        );

        stepped_over.continue_chunk(CHUNK_BUDGET);
        assert_ne!(
            stepped_over.get_pc(),
            line3_addr,
            "Continue must execute the breakpointed instruction and move off it"
        );
        assert_eq!(stepped_over.machine().get_register(2), 2);
    }
}
