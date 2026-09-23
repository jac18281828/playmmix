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
+------------------------------------------------------------------------+
| header playmmix[New][Share][Run][Continue][Step][Next][Interrupt][Res…]|
+--------------------------------+---------------------------------------+
| editor                         | machine status (PC, depth)            |
|   gutter | source              |---------------------------------------|
|   (existing pane, unchanged    | registers        (scroll)             |
|    behavior)                   |   $0  0x… (0)                         |
|                                |   $1  0x… (5)                         |
|                                |   …one per row…                       |
|                                |---------------------------------------|
|                                | special registers                     |
|                                |   rA  0x… (0)                         |
+--------------------------------+   …one per row…                       |
| output                (scroll) |---------------------------------------|
|   (program's stdout/stderr)    | memory           (scroll)             |
|                                |   text 0x…100  f2 00 …                |
+--------------------------------+---------------------------------------+
```

- New sits in the header, after the title: a plain, always-enabled button
  that starts over from the minimal skeleton. Unlike the run controls it
  never migrates to the touch-only fixed bar below, since starting over
  never depends on reaching a paused machine.
- Share sits after New: a plain, always-enabled button that puts the
  editor's program into a link and shares or copies it. Same rule as New,
  for the same reason -- sharing never depends on a paused machine either.
- The header also carries the run-state label and the status message, the
  last action's result; the diagram above drops both for width.
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
- On a touch device (`(hover: none) and (pointer: coarse)`) the control bar
  leaves the header and becomes `position: fixed`, clear of the notch and
  the left/right safe-area insets, and never scrolls away. Keyed on the
  input, not the viewport width, for the touch/non-touch split itself;
  which edge the bar pins to keys on the viewport too, on `(max-width:
  600px) or (max-height: 500px)` -- a phone, going by the app's own
  viewport rather than the device, so an iPad in a narrow Split View gets
  it too. The bar sits at the bottom on a full-screen iPad, where the
  screen is large enough that the top is the awkward reach; there, its own
  bottom padding clears the iOS home indicator. On a phone it sits at the
  top instead, where the bottom is not where a user looks for it, and the
  home indicator is `body`'s `padding-bottom` to clear, not the bar's.
  `--control-bar-h` and `--viewport-h` (`100dvh`, tracking Safari's
  collapsing toolbar) reserve the bar's space in `body`'s padding and
  `main`'s height, in both grid layouts, on whichever edge the bar sits.
  Inside the bar, the run-state label and status message sit on one line
  above a single row of six buttons, the status message truncated by an
  ellipsis rather than wrapping; the buttons carry no keyboard cue there,
  since a phone has no keyboard to teach.

Registers in a column, one per row, is the load-bearing change: it makes rows
addressable by position, which is what continuity (§ registers) and change
highlighting (§ highlights) hang off.

## Run lifecycle

Four states, driven by `Control`'s `running`, `halted`, and `session` flags:

| state    | Run | Continue | Step | Next | Interrupt | Reset | label     |
|----------|-----|----------|------|------|-----------|-------|-----------|
| ready    | ✓   | –        | ✓    | ✓    | –         | ✓     | `stopped` |
| running  | –   | –        | –    | –    | ✓         | –     | `running` |
| paused   | ✓   | ✓        | ✓    | ✓    | –         | ✓     | `paused`  |
| halted   | ✓   | –        | –    | –    | –         | ✓     | `halted`  |

- **Reset** re-runs `Control::reload` on the current source: fresh machine at
  the entry point, `halted` and the session cleared, breakpoints kept (they
  already survive reload by line number), output cleared. It is the "play
  again" control, and the start state Run always restarts through.
- **Run always restarts**: it re-runs Reset's own path first -- the fresh
  machine above -- then begins running from the entry, in every state,
  halted included. One click to replay, no beep. Step and Next stay disabled
  when halted: single-stepping from a halt is never what the user meant, and
  enabling them would silently replay from the top.
- **Continue** resumes a paused session in place, without restarting: it
  executes the instruction at the PC, then runs to a breakpoint or halt.
  Enabled only in `paused` -- gdb answers "The program is not being run"
  before a session starts, and `paused` is the only state a session is both
  started and not itself running.
- `paused` is a started session (Run, Step, or Next issued since the last
  load or Reset) that is neither running nor halted; it gets its own label
  so the state line distinguishes "never ran" from "stopped mid-run" --
  including a Run stopped at a breakpoint on the entry line, which starts a
  session even though nothing has executed yet.
- The halted state additionally shows `exit N` in the machine status line,
  as today.
- **Interrupt** is live only in `running` -- there is nothing to interrupt
  otherwise, and a live button that does nothing in `ready`/`paused` is the
  defect the owner found in Stop. Reset's own gate is the exact opposite --
  live everywhere but `running` -- so the two are never live together;
  Interrupt is the sole live control in `running`. Run shares Reset's gate
  too, so both are live in `halted`, not Reset alone.

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

The visible set is built from four rules, ascending index, one register per
row:

1. **Visible set:** a register renders when `sticky || value != 0 || i < rL
   || i >= rG`. Checksmix zeroes a register when an instruction makes it
   marginal (`PUSHJ`/`POP` in `push_frame`/`pop_frame`, `PUT rL`/`PUT rG` in
   `put_rl`/`put_rg`), and a fresh load starts every marginal at zero, so an
   unlisted marginal hides nothing. The rule governs, not the register's
   class: a nonzero marginal reachable only through a raw `rL`/`rG` restore
   (`UNSAVE`) still renders via `value != 0`.
2. **Globals:** every `$i >= rG` renders individually. A program with no
   `GREG` directive starts at `rG = 255`, so a fresh load has exactly one
   global, `$255`.
3. **Sticky:** any register observed rendering individually keeps its row for
   the life of the load, whatever its value does later — a register a `POP`
   or a `PUT rL`/`PUT rG` zeroes back to marginal stays listed, dimmed by the
   marginal styling, instead of vanishing. The sticky set lives in
   `machine::ViewState`, owned by `App` (it is view state, not machine
   state) and clears on Reset and reload.

   Stickiness is *sampled*, not tracked continuously: the visible set is
   observed at each pause boundary and each chunk yield, never per
   instruction while a chunked Run is in flight. A register that goes
   nonzero and reverts to zero entirely inside one chunk is therefore not
   guaranteed to be caught by Run. Step, which observes after every
   instruction, always catches it. This is an accepted consequence of
   chunked execution — sampling finer would trade away the responsiveness
   chunking exists for — not a defect in the rule.
4. **Global caption:** one `global · rG=N` row (U+00B7 middle dot) sits
   immediately before the first row at or above `rG`. It is a fact of the
   visible set, not a per-row tag: `rL` stays visible among the pinned
   special registers, teaching the same boundary from the other side.

A row never moves once shown; new rows insert in index order. General and
special registers share one row format: name, then hex, then the decimal in
parentheses directly after the hex — `$1  0x0000000000000002 (2)`, `rL
0x0000000000000001 (1)`. The decimal cell shows the signed integer, or, when
the bits are float-shaped (a biased exponent `923..=1123`, or either
infinity — see `pane.rs`'s `decimal_cell`), the float reading instead —
`$1  0x3FE0000000000000 (0.5)`, `$8  0x7FF0000000000000 (inf)`, `$9
0xFFF0000000000000 (-inf)`. A float reading never prints like an integer: a
finite value shows a decimal point or an exponent, with every digit needed
to round-trip — `(2.0)`, `(6.02e23)`, `(0.30000000000000004)` — and an
infinity prints as `(inf)` or `(-inf)`. NaN stays an integer: every integer
from -1 to -(2^52 - 1) has NaN bits, so reading NaN would misread that whole
range. A register carries no type, so the shape is a reading, not a fact: a
`Pool_Segment` pointer, `#4000000000000000`, reads as `(2.0)`. A float
reading's span carries the value as a signed 64-bit integer in its title,
`title="as an integer: 4602678819172646912"`; an integer reading carries no
title. Fixed `ch` widths for
name (4ch, right-aligned) and hex (18ch) let a value update in place without
reflow; the decimal takes its natural width, left-aligned, rather than
reserving space for a value it isn't showing. Special-register rows are the
six pinned ones (`rA rG rL rO rS rJ`) first, then any other nonzero special,
sticky under the same rule. A fresh load already shows `rK`, `rT`, `rTT` and
`rV`: checksmix starts each of them nonzero.

A row wider than its pane scrolls inside that pane (`.registers-scroll`),
never the page — the owner's choice. At the pane's font a decimal cell fits
17 characters with its parentheses on a 375px phone; a wider cell, integer
or float, scrolls inside the pane instead of widening it. A full-precision
float reading usually exceeds it — `(0.16666666666666666)` is 21 characters
— and the widest, `(-1.5777218104420234e-30)`, is 25.

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
   marker on the instruction that halted — a `TRAP` or the faulting
   instruction, not whatever the live PC now points at.
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
  sticky sets and the previous-render snapshot, both in `machine::
  ViewState`, owned by `App`.
