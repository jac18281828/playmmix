//! Shared MMIXAL fixtures used by tests in more than one machine file.

/// Two `GREG`s, one initialized to a literal zero -- verified against
/// checksmix `main` while authoring the dispatch prompt: `rG = 253`,
/// `rL = 0`. `$254` (from `G1 GREG 0`) is zero and `$255` holds `Main`'s
/// entry address (`start_program`'s start state); both must still show,
/// because clause 3 (`i >= rG`) marks them global regardless of value.
pub(super) const TWO_GREG_MMS: &str =
    "\tLOC\t#100\nG1\tGREG\t0\nG2\tGREG\t@\nMain\tTRAP\t0,Halt,0\n";
/// Same fixture as `control.rs`'s `CALL_MMS`: no `GREG` at all; `SET
/// $255,$0` writes a nonzero value into `$255` before `TRAP 0,Halt,0`.
pub(super) const CALL_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,40\n\tSETL\t$2,2\n\tPUSHJ\t$0,AddFunc\n\tSET\t$255,$0\n\tTRAP\t0,Halt,0\nAddFunc\tADDU\t$0,$0,$1\n\tPOP\t1,0\n";
/// `SETL $40,7` raises `rL` to 41 before writing it, so `$40` renders
/// individually (local, `i < rL`) and goes sticky; `PUTI rL,0` then drops
/// `rL` back to 0, which checksmix's `put_rl` zeroes `$40` for, marginal and
/// unwritten again by the time `TRAP 0,Halt,0` halts. Exercises a register
/// that stops satisfying the plain visibility rule but keeps rendering
/// through stickiness.
pub(super) const REVERTING_MARGINAL_MMS: &str =
    "\tLOC\t#100\nMain\tSETL\t$40,7\n\tPUTI\trL,0\n\tTRAP\t0,Halt,0\n";
