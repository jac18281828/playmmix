//! Loaded memory: runs, islands, and the aligned display rows.

use std::collections::{BTreeMap, HashMap};

use checksmix::MMix;

/// Bytes shown per memory row: wide enough to read a short string at a
/// glance, narrow enough to fit one line. Every row is aligned to this
/// width, per `docs/layout-spec.md`'s Memory pane section.
pub(super) const MEMORY_ROW_WIDTH: usize = 16;

/// One of the address space's segments, selected by an address's top three
/// bits: Text, Data, Pool and Stack are MMIX's own four; an address with
/// the sign bit set (`>= #8000000000000000`) belongs to the operating
/// system, labeled `Os`. checksmix doesn't export these boundaries
/// (`control.rs` restates `DATA_SEGMENT_START` the same way); they're
/// stable MMIX architectural constants, safe to restate here too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    Text,
    Data,
    Pool,
    Stack,
    Os,
}

impl Segment {
    fn from_addr(addr: u64) -> Self {
        match addr >> 61 {
            0 => Segment::Text,
            1 => Segment::Data,
            2 => Segment::Pool,
            3 => Segment::Stack,
            _ => Segment::Os,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Segment::Text => "text",
            Segment::Data => "data",
            Segment::Pool => "pool",
            Segment::Stack => "stack",
            Segment::Os => "os",
        }
    }
}

/// One contiguous run of loaded memory within a single segment, tagged
/// with any labels naming an address inside it.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRun {
    pub segment: Segment,
    pub start: u64,
    pub bytes: Vec<u8>,
    /// Labels landing inside this run, in ascending address order.
    pub labels: Vec<(u64, String)>,
}

/// `loaded_extent()`'s addresses -- what `write_image` loaded, including a
/// byte the program set to zero, which `occupied()`'s sparse-memory view
/// would drop -- collapsed into contiguous per-segment runs and tagged
/// with `labels` entries landing inside them.
pub fn memory_runs(mmix: &MMix, labels: &HashMap<String, u64>) -> Vec<MemoryRun> {
    let mut runs: Vec<MemoryRun> = Vec::new();

    for (addr, byte) in mmix.loaded_extent() {
        let segment = Segment::from_addr(addr);
        match runs.last_mut() {
            Some(run)
                if run.segment == segment
                    && (run.start + (run.bytes.len() as u64 - 1)).checked_add(1) == Some(addr) =>
            {
                run.bytes.push(byte);
            }
            _ => runs.push(MemoryRun {
                segment,
                start: addr,
                bytes: vec![byte],
                labels: Vec::new(),
            }),
        }
    }

    for (name, &addr) in labels {
        if let Some(run) = runs.iter_mut().find(|run| {
            let last = run.start + (run.bytes.len() as u64 - 1);
            addr >= run.start && addr <= last
        }) {
            run.labels.push((addr, name.clone()));
        }
    }
    for run in &mut runs {
        run.labels.sort();
    }

    runs
}

/// One displayed row: either 16 aligned cells of a merged run (a real byte
/// as `Some`, a padding cell as `None`, never `00` for padding), or a thin
/// marker between two runs in different segments.
#[derive(Debug, Clone, PartialEq)]
pub enum MemoryRow {
    Data {
        segment: Segment,
        addr: u64,
        cells: [Option<u8>; MEMORY_ROW_WIDTH],
        labels: Vec<String>,
    },
    SegmentBreak,
}

/// A same-segment stretch of one or more [`MemoryRun`]s, merged when a gap
/// between consecutive runs is smaller than one row -- the alignment unit
/// [`memory_rows`] chunks into display rows.
struct MemoryIsland {
    segment: Segment,
    start: u64,
    /// The island's last byte address, inclusive -- one past it can be
    /// `u64::MAX + 1`, which `u64` cannot hold.
    end: u64,
    bytes: BTreeMap<u64, u8>,
    labels: Vec<(u64, String)>,
}

/// Merge same-segment runs whose gap is smaller than one row, per
/// `docs/layout-spec.md`'s Memory pane section: row identity is the aligned
/// address, so two runs that would otherwise land on the same aligned row
/// must share one island rather than each claiming it independently.
/// Different-segment runs never merge -- MMIX segments sit far enough
/// apart that a gap that size never occurs between them.
fn merge_islands(runs: &[MemoryRun]) -> Vec<MemoryIsland> {
    let mut islands: Vec<MemoryIsland> = Vec::new();

    for run in runs {
        let run_end = run.start + (run.bytes.len() as u64 - 1);
        let width = MEMORY_ROW_WIDTH as u64;

        match islands.last_mut() {
            Some(last)
                if last.segment == run.segment
                    && run.start > last.end
                    && last
                        .end
                        .checked_add(width)
                        .is_none_or(|limit| run.start <= limit) =>
            {
                last.end = run_end;
                for (i, &byte) in run.bytes.iter().enumerate() {
                    last.bytes.insert(run.start + i as u64, byte);
                }
                last.labels.extend(run.labels.iter().cloned());
            }
            _ => {
                let bytes = run
                    .bytes
                    .iter()
                    .enumerate()
                    .map(|(i, &byte)| (run.start + i as u64, byte))
                    .collect();
                islands.push(MemoryIsland {
                    segment: run.segment,
                    start: run.start,
                    end: run_end,
                    bytes,
                    labels: run.labels.clone(),
                });
            }
        }
    }

    islands
}

/// Chunk each run into 16-byte-aligned display rows. A run whose start or
/// end doesn't land on the boundary pads with blank (`None`) cells; a
/// segment change between islands gets a [`MemoryRow::SegmentBreak`].
pub fn memory_rows(runs: &[MemoryRun]) -> Vec<MemoryRow> {
    let mut rows = Vec::new();
    let mut last_segment: Option<Segment> = None;

    for island in merge_islands(runs) {
        if last_segment.is_some_and(|segment| segment != island.segment) {
            rows.push(MemoryRow::SegmentBreak);
        }
        last_segment = Some(island.segment);

        let width = MEMORY_ROW_WIDTH as u64;
        let aligned_start = island.start - island.start % width;

        let mut row_start = aligned_start;
        loop {
            let mut cells = [None; MEMORY_ROW_WIDTH];
            for (offset, cell) in cells.iter_mut().enumerate() {
                *cell = island.bytes.get(&(row_start + offset as u64)).copied();
            }
            // The row's own last address, inclusive -- `row_start + width`
            // would be one past it, which overflows for the top row.
            let row_last = row_start + (width - 1);
            let labels = island
                .labels
                .iter()
                .filter(|(addr, _)| *addr >= row_start && *addr <= row_last)
                .map(|(_, name)| name.clone())
                .collect();
            rows.push(MemoryRow::Data {
                segment: island.segment,
                addr: row_start,
                cells,
                labels,
            });

            if row_last >= island.end {
                break;
            }
            row_start += width;
        }
    }

    rows
}

/// Whether `row` is the current-instruction row: `marker_pc` names a real,
/// non-padding byte inside it. Never true for a padding cell or a
/// [`MemoryRow::SegmentBreak`].
pub fn memory_row_is_current(row: &MemoryRow, marker_pc: u64) -> bool {
    match row {
        MemoryRow::Data { addr, cells, .. } => {
            marker_pc >= *addr
                && addr
                    .checked_add(MEMORY_ROW_WIDTH as u64)
                    .is_none_or(|end| marker_pc < end)
                && cells[(marker_pc - addr) as usize].is_some()
        }
        MemoryRow::SegmentBreak => false,
    }
}

/// Cell offsets inside `row` covered by the 4-byte instruction span starting
/// at `marker_pc`. MMIX instructions are tetra-aligned, so a span can never
/// straddle a 16-byte row boundary; empty unless `memory_row_is_current`
/// holds for the same `row`/`marker_pc`.
pub fn memory_row_instruction_span(row: &MemoryRow, marker_pc: u64) -> Vec<usize> {
    let MemoryRow::Data { addr, cells, .. } = row else {
        return Vec::new();
    };
    (0..4u64)
        .filter_map(|i| {
            let byte_addr = marker_pc.checked_add(i)?;
            let past_row_end = addr
                .checked_add(MEMORY_ROW_WIDTH as u64)
                .is_some_and(|end| byte_addr >= end);
            if byte_addr < *addr || past_row_end {
                return None;
            }
            let offset = (byte_addr - addr) as usize;
            cells[offset].is_some().then_some(offset)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use checksmix::{MMixAssembler, write_image};

    #[test]
    fn from_addr_maps_every_boundary_including_the_sign_bit() {
        assert_eq!(Segment::from_addr(0x5FFF_FFFF_FFFF_FFFF), Segment::Pool);
        assert_eq!(Segment::from_addr(0x6000_0000_0000_0000), Segment::Stack);
        assert_eq!(Segment::from_addr(0x7FFF_FFFF_FFFF_FFFF), Segment::Stack);
        assert_eq!(Segment::from_addr(0x8000_0000_0000_0000), Segment::Os);
        assert_eq!(Segment::from_addr(u64::MAX), Segment::Os);
    }

    /// `LOC #FFFFFFFFFFFFFFFC` followed by an instruction under a label
    /// places that instruction's last byte at `u64::MAX` and its start
    /// (`#...FC`) off the 16-byte boundary -- the case that reaches
    /// `memory_runs`' label-membership sum, `merge_islands`' run end, and
    /// `memory_rows`' alignment and row loop at the very top of the address
    /// space, with no panic and no wrapped row. `SETL $1,#0203` encodes as
    /// `e3 01 02 03` -- four distinct bytes, so a cell landing in the wrong
    /// offset shows up as a wrong value, not just a wrong presence.
    const TOP_OF_ADDRESS_SPACE_MMS: &str = "\tLOC\t#FFFFFFFFFFFFFFFC\nMain\tSETL\t$1,#0203\n";

    #[test]
    fn memory_models_the_last_byte_of_the_address_space_exactly() {
        let mut assembler = MMixAssembler::new(TOP_OF_ADDRESS_SPACE_MMS, "top.mms");
        assembler
            .parse()
            .expect("an instruction at #FFFFFFFFFFFFFFFC must assemble");
        let mut mmix = MMix::new();
        write_image(&mut mmix, &assembler);

        let main_addr = *assembler
            .labels
            .get("Main")
            .expect("top.mms defines a Main label");
        assert_eq!(main_addr, u64::MAX - 3);

        let runs = memory_runs(&mmix, &assembler.labels);
        let run = runs
            .iter()
            .find(|run| run.start == main_addr)
            .expect("a run starting at Main");
        assert_eq!(run.segment, Segment::Os);
        assert_eq!(run.bytes.len(), 4, "one tetra instruction, 4 bytes");
        assert_eq!(
            run.labels,
            vec![(main_addr, "Main".to_string())],
            "the label pass' membership test must place Main inside a run \
             whose last byte is u64::MAX"
        );

        let rows = memory_rows(&runs);
        let top_row = rows.last().expect("at least one row");
        let MemoryRow::Data {
            addr,
            cells,
            labels,
            ..
        } = top_row
        else {
            panic!("expected the top row to be a data row");
        };
        assert_eq!(
            *addr,
            main_addr - main_addr % MEMORY_ROW_WIDTH as u64,
            "the top row aligns down from the run's misaligned start"
        );
        const EXPECTED_BYTES: [u8; 4] = [0xe3, 0x01, 0x02, 0x03];
        for (offset, cell) in cells.iter().enumerate() {
            let byte_addr = *addr + offset as u64;
            if byte_addr >= main_addr {
                assert_eq!(
                    *cell,
                    Some(EXPECTED_BYTES[(byte_addr - main_addr) as usize]),
                    "byte {byte_addr:#x} at the top of the address space must land \
                     in its own cell"
                );
            } else {
                assert_eq!(
                    *cell, None,
                    "padding before the run's start must stay blank"
                );
            }
        }
        assert_eq!(labels, &vec!["Main".to_string()]);

        assert!(memory_row_is_current(top_row, main_addr));
        assert!(memory_row_is_current(top_row, u64::MAX));
        assert_eq!(
            memory_row_instruction_span(top_row, main_addr),
            vec![12, 13, 14, 15],
            "the instruction's four cells at the top row must all name a PC \
             inside it, with no overflow computing the row's end"
        );
    }

    #[test]
    fn memory_runs_tag_the_text_label_with_its_full_loaded_bytes() {
        let mut assembler = MMixAssembler::new(crate::examples::HELLO_WORLD_MMS, "hello.mms");
        assembler.parse().expect("hello_world.mms must assemble");
        let mut mmix = MMix::new();
        write_image(&mut mmix, &assembler);

        let text_addr = *assembler
            .labels
            .get("Text")
            .expect("hello_world.mms defines a Text label");
        assert_eq!(text_addr, 0x2000_0000_0000_0000);

        let runs = memory_runs(&mmix, &assembler.labels);
        let run = runs
            .iter()
            .find(|run| {
                run.labels
                    .iter()
                    .any(|(addr, name)| *addr == text_addr && name == "Text")
            })
            .expect("a run must be tagged with the Text label");

        // 14 bytes via loaded_extent(), including the trailing 0 that
        // occupied() would drop.
        let offset = (text_addr - run.start) as usize;
        assert_eq!(&run.bytes[offset..offset + 14], b"Hello world!\n\0");
    }

    /// Stores a nonzero byte to `#1000` -- a text address this program's
    /// own `LOC` output never touches -- then halts. checksmix's
    /// `write_byte` drops a zero byte from the sparse memory map ("Don't
    /// store zeros", an inline comment in its `mix/memory.rs`, not a public
    /// doc), so the store must write something nonzero for `occupied()` to
    /// actually grow.
    const STORES_A_BYTE_AT_A_NEW_ADDRESS_MMS: &str =
        "\tLOC\t#100\nMain\tSETL\t$1,1\n\tSETL\t$2,#1000\n\tSTBU\t$1,$2,0\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn memory_runs_stay_fixed_across_a_real_run_via_loaded_extent() {
        let mut control =
            crate::control::Control::new(STORES_A_BYTE_AT_A_NEW_ADDRESS_MMS, "store.mms")
                .expect("assembles");

        let before = memory_runs(control.machine(), control.labels());
        let before_len: usize = before.iter().map(|run| run.bytes.len()).sum();
        let occupied_before = control.machine().occupied().count();

        let outcome = control.run_chunk(1_000_000);
        assert_eq!(outcome, crate::control::StepOutcome::Halted);
        assert!(control.is_halted(), "the run must actually reach a halt");

        let occupied_after = control.machine().occupied().count();
        assert!(
            occupied_after > occupied_before,
            "occupied() must grow across the run -- guards against a vacuous \
             pass where nothing actually executed"
        );

        let after = memory_runs(control.machine(), control.labels());
        let after_len: usize = after.iter().map(|run| run.bytes.len()).sum();

        // 4 instructions, 4 bytes each, no data segment -- unchanged by the
        // run, since loaded_extent() tracks only what write_image loaded,
        // not the run-time store to #1000.
        assert_eq!(before_len, 16);
        assert_eq!(after_len, before_len);
    }

    #[test]
    fn memory_rows_pad_a_misaligned_run_start() {
        let run = MemoryRun {
            segment: Segment::Text,
            start: 0x104,
            bytes: vec![0xAA; 4],
            labels: Vec::new(),
        };
        let rows = memory_rows(&[run]);
        let MemoryRow::Data { addr, cells, .. } = &rows[0] else {
            panic!("expected a data row");
        };
        assert_eq!(
            *addr, 0x100,
            "row address must align down to the 16-byte boundary"
        );
        for cell in &cells[0..4] {
            assert_eq!(*cell, None, "leading padding cell must be blank, not 0x00");
        }
        for cell in &cells[4..8] {
            assert_eq!(*cell, Some(0xAA));
        }
    }

    #[test]
    fn memory_rows_pad_a_short_trailing_row() {
        let run = MemoryRun {
            segment: Segment::Text,
            start: 0x100,
            // Spans two rows: 0x100..0x110 full, 0x110..0x114 partial.
            bytes: vec![0xBB; 20],
            labels: Vec::new(),
        };
        let rows = memory_rows(&[run]);
        assert_eq!(rows.len(), 2);
        let MemoryRow::Data { addr, cells, .. } = &rows[1] else {
            panic!("expected a data row");
        };
        assert_eq!(*addr, 0x110);
        for cell in &cells[0..4] {
            assert_eq!(*cell, Some(0xBB));
        }
        for cell in &cells[4..16] {
            assert_eq!(*cell, None, "trailing padding cell must be blank, not 0x00");
        }
    }

    #[test]
    fn memory_rows_merge_close_runs_in_the_same_segment() {
        let run_a = MemoryRun {
            segment: Segment::Data,
            start: 0x2000_0000_0000_0000,
            bytes: vec![1, 2, 3],
            labels: Vec::new(),
        };
        // A 5-byte gap (bytes 3..8), well under one 16-byte row.
        let run_b = MemoryRun {
            segment: Segment::Data,
            start: 0x2000_0000_0000_0008,
            bytes: vec![9, 9],
            labels: Vec::new(),
        };
        let rows = memory_rows(&[run_a, run_b]);
        assert_eq!(rows.len(), 1, "both runs fit in one merged 16-byte row");

        let MemoryRow::Data { cells, .. } = &rows[0] else {
            panic!("expected a data row");
        };
        assert_eq!(cells[0], Some(1));
        assert_eq!(
            cells[3], None,
            "the gap between runs must render as padding"
        );
        assert_eq!(cells[8], Some(9));
    }

    #[test]
    fn memory_row_flags_the_current_instruction_and_its_span() {
        let run = MemoryRun {
            segment: Segment::Text,
            start: 0x100,
            bytes: vec![0; 16],
            labels: Vec::new(),
        };
        let rows = memory_rows(&[run]);
        let row = &rows[0];

        let marker_pc = 0x108;
        assert!(memory_row_is_current(row, marker_pc));
        assert_eq!(
            memory_row_instruction_span(row, marker_pc),
            vec![8, 9, 10, 11]
        );

        let other_run = MemoryRun {
            segment: Segment::Text,
            start: 0x200,
            bytes: vec![0; 16],
            labels: Vec::new(),
        };
        let other_rows = memory_rows(&[other_run]);
        assert!(!memory_row_is_current(&other_rows[0], marker_pc));
        assert!(memory_row_instruction_span(&other_rows[0], marker_pc).is_empty());
    }

    #[test]
    fn memory_row_marker_uses_the_halted_adjustment() {
        // Mirrors Control::marker_pc: while halted, the marker names the
        // instruction that halted, not the live PC past it -- a TRAP halt
        // advances the PC 4 bytes past that instruction, the case this
        // example models. The memory pane must flag the halting
        // instruction's row, not the (past-the-end) raw PC's.
        let run = MemoryRun {
            segment: Segment::Text,
            start: 0x100,
            bytes: vec![0; 16],
            labels: Vec::new(),
        };
        let rows = memory_rows(&[run]);
        let row = &rows[0];

        let raw_pc_after_halt = 0x110; // past this run entirely
        let halted_marker_pc = raw_pc_after_halt - 4; // the halting TRAP's own address

        assert!(
            !memory_row_is_current(row, raw_pc_after_halt),
            "the raw halted PC must not flag any row here"
        );
        assert!(memory_row_is_current(row, halted_marker_pc));
        assert_eq!(
            memory_row_instruction_span(row, halted_marker_pc),
            vec![12, 13, 14, 15]
        );
    }
}
