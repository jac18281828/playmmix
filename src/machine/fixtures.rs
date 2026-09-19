//! Shared MMIXAL fixtures used by tests in more than one machine file.

/// Same fixture as `control.rs`'s `CALL_MMS`: no `GREG` at all, but
/// `SET $255,$0` writes a nonzero value into a register above the
/// no-GREG collapse floor before `TRAP 0,Halt,0`.
pub(super) const CALL_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,40\n\tSETL\t$2,2\n\tPUSHJ\t$0,AddFunc\n\tSET\t$255,$0\n\tTRAP\t0,Halt,0\nAddFunc\tADDU\t$0,$0,$1\n\tPOP\t1,0\n";
/// Two `GREG`s, one initialized to a literal zero -- verified against
/// checksmix `main` while authoring the dispatch prompt: `rG = 253`,
/// `rL = 0`. `$254` (from `G1 GREG 0`) is zero and `$255` holds `Main`'s
/// entry address (`start_program`'s start state); both must still show,
/// because clause 3 (`i >= rG`) marks them global regardless of value.
pub(super) const TWO_GREG_MMS: &str =
    "\tLOC\t#100\nG1\tGREG\t0\nG2\tGREG\t@\nMain\tTRAP\t0,Halt,0\n";
/// No `GREG` at all, and the only register it touches is `$40` -- above
/// `rG`'s default of 32, so `set_register` never grows `rL` and
/// `register_included`'s `i < rL` clause can't keep `$40` visible on
/// its own. `$40` goes nonzero and reverts within three instructions,
/// far inside one `CHUNK_BUDGET`.
pub(super) const REVERTING_GLOBAL_MMS: &str =
    "\tLOC\t#100\nMain\tSETL\t$40,7\n\tSETL\t$40,0\n\tTRAP\t0,Halt,0\n";
