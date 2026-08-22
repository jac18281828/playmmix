//! Bundled `.mms` source: the minimal skeleton the editor loads on first
//! paint, and a richer worked example kept as a test fixture.

/// The program the editor loads on first visit: the minimal skeleton every
/// MMIX program needs -- an entry point that halts cleanly, and an empty
/// `Data_Segment` with `GREG @` ready for whatever a user adds -- rather
/// than a worked example. `debug`-pseudo-op walkthroughs and multi-word
/// pseudo-ops step confusingly (they expand into several physical
/// instructions, only the first of which maps back to its source line);
/// this has neither, so Step always lands on the next source line.
///
/// Must assemble: `App::create` loads it unconditionally. Pinned by
/// `default_mms_assembles` below.
pub const DEFAULT_MMS: &str = "% ------------------------------------------------------------\n% mmix.mms -- minimal starting point for an MMIX program\n% ------------------------------------------------------------\n\n\tLOC\t#100\t\t\t% code segment start\nMain\tTRAP\t0,Halt,0\t\t% exit\n\n\tLOC\tData_Segment\n\tGREG\t@\n";

/// `examples/hello_world.mms` from checksmix, embedded verbatim. Not
/// `include_str!`: a dependency's on-disk location, wherever cargo puts it
/// (crates.io registry cache or git checkout alike), is not a stable,
/// crate-relative path. Not the editor's default program (see
/// `DEFAULT_MMS`) -- kept for its own sake as a behaviorally rich test
/// fixture (`debug`, `LDA`, `Fputs`, a data-segment string) across
/// `control.rs` and `machine.rs`, which is its only remaining use --
/// `#[cfg(test)]` accordingly, or it's dead code in a release build.
#[cfg(test)]
pub const HELLO_WORLD_MMS: &str = "\tLOC\tData_Segment\n\tGREG\t@\nText\tBYTE\t\"Hello world!\",'\\n',0\n\n\tLOC\t#100\n\nMain\tdebug \"Version 0.1: Hello World Example\"\t\n\tLDA\t\t$255,Text\n\tTRAP\t0,Fputs,StdOut\n\tTRAP\t0,Halt,0\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mms_assembles() {
        crate::control::Control::new(DEFAULT_MMS, "default.mms").expect("must assemble");
    }

    #[test]
    fn hello_world_mms_assembles() {
        crate::control::Control::new(HELLO_WORLD_MMS, "hello.mms").expect("must assemble");
    }
}
