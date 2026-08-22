# playmmix screen layout — design specification

Status: draft for review. Covers layout, pane contents, run lifecycle and
display stability. Grounded in the code as of `a12c612` (0.1.2): `main.rs`
renders a single vertical stack; `machine.rs` renders registers in a
multi-column `auto-fill` grid; `control.rs` has no Reset and `MMix::new()`
discards program output.

## Problems this spec resolves

1. After a halt every button disables (`idle_only = running || halted` with
   no Reset), so the only way to run again is to edit the source. Dead end.
2. Register rows render in a multi-column grid of `nowrap` flex rows; at
   real widths the columns overlap and overwrite each other. Unreadable.
3. Nothing marks where execution is: the gutter highlight exists but there
   is no marker in the memory pane, and nothing marks what changed.
4. Program output is lost. `Control::assemble_and_load` builds `MMix::new()`,
   whose default host writes to stdout — nowhere, in wasm. checksmix already
   provides the fix: `MMix::with_host` and the `Host` trait.
5. The register set is value-driven, so registers appear and vanish between
   steps. The eye can't track a value that moves rows every render.
6. Data-segment rows start at the run's own start address, so their columns
   don't line up with text-segment rows.

## Screen regions

Two-column split on desktop, replacing today's stack. Left is the program;
right is the machine. CSS grid with named areas on `<main>`:

```
+--------------------------------------------------------------+
| header:  playmmix   [Run][Step][Step Over][Stop][Reset] state|
+--------------------------------+-----------------------------+
| editor                         | machine status (PC, depth)  |
|   gutter | source              |-----------------------------|
|   (existing pane, unchanged    | registers        (scroll)   |
|    behavior)                   |   $0  0x…  0                |
|                                |   $1  0x…  5                |
|                                |   …one per row…             |
|                                |-----------------------------|
|                                | special registers           |
|                                |   rA  0x…  0                |
+--------------------------------+   …one per row…             |
| output                (scroll) |-----------------------------|
|   Hello world!                 | memory           (scroll)   |
|                                |   text 0x…100  f2 00 …      |
+--------------------------------+-----------------------------+
```

- `grid-template-columns: minmax(0, 1fr) minmax(38rem, 42rem)` — the machine
  column is sized by its content (fixed-width rows, below); the editor takes
  the rest.
- Left column: editor above, output below, `grid-template-rows: minmax(8rem,
  1fr) auto` with the output pane at `max-height: 14rem`.
- Under ~1100px the grid collapses to one column: header, editor, output,
  machine. The machine column's own order is already vertical, so nothing
  else changes.
- Each scrolling pane (`registers`, `memory`, `output`) owns its scrollbar:
  `overflow-y: auto` on the pane, never on `.machine-pane` as a whole. The
  page itself never scrolls the machine state out from under the editor.
- The `grid-template-columns`/`grid-template-rows` values above are the
  *default* sizing, not fixed proportions: both boundaries -- the column
  split between the editor+output column and the machine column, and the
  row split between the editor and the output pane -- are user-draggable,
  clamped to floors (20rem for the left column, the machine column's own
  38rem floor, a couple of lines plus its header for the output pane) that
  keep every pane usable. A committed drag also carries a `min(px,
  calc(...))` CSS ceiling so a later window resize, with no further drag,
  re-clamps it on every reflow rather than overflowing at the stale pixel
  value. Horizontally this ceiling is exact; vertically it can't account for
  the header row's own (`auto`, wrappable) height, so the editor's `minmax(
  8rem, 1fr)` floor is what actually guarantees the editor pane stays
  visible. Below roughly a 420px window height the editor holds at that
  128px floor rather than shrinking further, and the page scrolls instead --
  a real tradeoff, not a tightly bounded residual: the overflow grows
  somewhat as the window keeps shrinking (measured up to ~240px at a 200px
  window height, undragged), but the editor never disappears, which an
  unguarded `1fr` row would allow.

Registers in a column, one per row, is the load-bearing change: it makes rows
addressable by position, which is what continuity (§ registers) and change
highlighting (§ highlights) hang off.

## Run lifecycle

Four states, driven by the two flags `Control` already has plus the new
Reset:

| state    | Run | Step | Step Over | Stop | Reset | label     |
|----------|-----|------|-----------|------|-------|-----------|
| ready    | ✓   | ✓    | ✓         | –    | ✓     | `stopped` |
| running  | –   | –    | –         | ✓    | –     | `running` |
| paused   | ✓   | ✓    | ✓         | –    | ✓     | `paused`  |
| halted   | ✓   | –    | –         | –    | ✓     | `halted`  |

- **Reset** re-runs `Control::reload` on the current source: fresh machine at
  the entry point, `halted` cleared, breakpoints kept (they already survive
  reload by line number), output cleared. It is the "play again" control.
- **Run while halted** performs Reset then Run — one click to replay, no
  beep. Step and Step Over stay disabled when halted: single-stepping from a
  halt is never what the user meant, and enabling them would silently replay
  from the top.
- `paused` is today's `stopped`-after-a-breakpoint/Step; it gets its own
  label so the state line distinguishes "never ran" from "stopped mid-run".
- The halted state additionally shows `exit N` in the machine status line,
  as today.

## Output pane

New pane, left column, under the editor. Requires a capture host: a `Host`
implementation whose `Fputs`/`Fwrite` append to a shared `Rc<RefCell<String>>`
(stdout and stderr interleaved in arrival order, stderr spans tagged with
their own class), passed to `MMix::with_host` in `assemble_and_load`.

- Monospace, `white-space: pre-wrap`, own scrollbar, pinned to bottom while
  new output arrives; a user scroll-up unpins until they return to bottom.
- Cleared by Reset and by reload (an edit). Never cleared mid-run.
- During a chunked run the pane updates at chunk boundaries — the same
  cadence the machine pane already repaints at. No per-write render.
- Header row: `OUTPUT` plus, once halted, `exit N` mirrored from the status
  line so the result of the run reads in one place.

## Registers

Replace the value-driven visible set with a stable one. Three rules, applied
in order, ascending index, one register per row:

1. **Pinned floor:** `$0`–`$31` always render, zero or not. This is the
   local-register file as MMIX teaches it, and it gives every program the
   same first 32 rows every time.
2. **Globals:** every `$i >= rG` renders, as today. The no-`GREG` collapse
   row (`$32–$254 · 223 unallocated (0)`) survives for the untouched middle,
   but now only ever covers `$32..=$255`.
3. **Sticky:** any register that has rendered individually keeps its row for
   the life of the load, whatever its value does later. The sticky set lives
   in `App` (it is view state, not machine state) and clears on Reset and
   reload.

A row never moves once shown; new rows insert in index order. Fixed column
widths in `ch` so a value updates in place without reflow: name 5ch
right-aligned, hex 18ch, decimal right-aligned in the remainder. Same row
format for special registers: the six pinned ones (`rA rG rL rO rS rJ`)
first, then any other nonzero special, sticky under the same rule.

## Memory pane

- Own scrollbar (`overflow-y: auto`), flexes to the remaining machine-column
  height.
- **Aligned rows:** every row starts at a 16-byte boundary. A run whose
  start isn't aligned pads its first row with blank cells (rendered as
  spaces, not `00`) down to the boundary. Text and data segments then share
  identical columns: segment 5ch, address 18ch, hex 16×3ch, ASCII 16ch,
  label. This is the "same layout, same width" fix — one ruler for all
  segments.
- Segment breaks get a thin separator row rather than interleaving.
- Row identity is its aligned start address, so a row's bytes update in
  place across steps, same stability rule as registers.

## Highlights

Three markers, all driven by state `App` already holds or can diff at render:

1. **Current instruction, editor:** the existing `gutter-current` line,
   unchanged — hidden mid-run, shown when ready/paused/halted.
2. **Current instruction, memory:** the memory row containing the PC gets
   `mem-current` (same background as `gutter-current`), and within the row
   the 4-byte instruction span gets the accent color. Halted keeps the
   marker on the halting TRAP: the last thing that ran stays visible.
3. **Changed since last pause:** registers, specials and memory bytes whose
   value differs from the previous paused render get a `changed` class
   (accent text, no background), cleared on the next advance. Diffing is a
   compare of the previous snapshot `App` keeps for exactly this purpose —
   no machine-side delta tracking. Mid-run chunk repaints skip the diff;
   "changed" means changed by the step or run segment that just finished,
   which is the question the user is actually asking.

## Out of scope

A user-facing horizontal/vertical layout toggle. The two-column split with a
narrow-viewport collapse covers both orientations without a control to
maintain; a toggle earns its place only if the collapse breakpoint proves
wrong in use. Also out: memory editing, register editing, follow-PC
autoscroll in the memory pane (revisit after the aligned rows land).

## Implementation notes

- Layout and CSS changes touch `style.css` and the `view` functions only;
  pane logic stays host-testable per `AGENTS.md` (sticky sets, aligned-row
  chunking and snapshot diffing are plain functions with `cargo test`
  coverage, same as `visible_registers` today).
- The capture host is the one change with a checksmix seam: `with_host`
  consumes the host, so the shared buffer handle is the only way back to the
  output — hold the `Rc` in `Control`.
- Reset reuses `reload` verbatim; the only new control-plane state is the
  sticky sets and the previous-render snapshot, both in `App`.
