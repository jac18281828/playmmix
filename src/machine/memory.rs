//! Loaded memory: runs, islands, and the aligned display rows.

use std::collections::{BTreeMap, HashMap};

use checksmix::MMix;

/// Bytes shown per memory row: wide enough to read a short string at a
/// glance, narrow enough to fit one line. Every row is aligned to this
/// width, per `docs/layout-spec.md`'s Memory pane section.
pub(super) const MEMORY_ROW_WIDTH: usize = 16;

/// One of MMIX's four segments, selected by an address's top three bits.
/// checksmix doesn't export this constant (`control.rs` restates
/// `DATA_SEGMENT_START` the same way); it's a stable MMIX architectural
/// boundary, safe to restate here too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    Text,
    Data,
    Pool,
    Stack,
}

impl Segment {
    fn from_addr(addr: u64) -> Self {
        match addr >> 61 {
            0 => Segment::Text,
            1 => Segment::Data,
            2 => Segment::Pool,
            _ => Segment::Stack,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Segment::Text => "text",
            Segment::Data => "data",
            Segment::Pool => "pool",
            Segment::Stack => "stack",
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
        let extends = runs.last().is_some_and(|run| {
            run.segment == segment && run.start + run.bytes.len() as u64 == addr
        });
        if extends {
            runs.last_mut()
                .expect("just checked non-empty")
                .bytes
                .push(byte);
        } else {
            runs.push(MemoryRun {
                segment,
                start: addr,
                bytes: vec![byte],
                labels: Vec::new(),
            });
        }
    }

    for (name, &addr) in labels {
        if let Some(run) = runs
            .iter_mut()
            .find(|run| addr >= run.start && addr < run.start + run.bytes.len() as u64)
        {
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
        let run_end = run.start + run.bytes.len() as u64;
        let merges = islands.last().is_some_and(|last| {
            last.segment == run.segment
                && run.start >= last.end
                && run.start - last.end < MEMORY_ROW_WIDTH as u64
        });

        if merges {
            let last = islands.last_mut().expect("just checked non-empty");
            last.end = run_end;
            for (i, &byte) in run.bytes.iter().enumerate() {
                last.bytes.insert(run.start + i as u64, byte);
            }
            last.labels.extend(run.labels.iter().cloned());
        } else {
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
        let aligned_end = island.end.div_ceil(width) * width;

        let mut row_start = aligned_start;
        while row_start < aligned_end {
            let mut cells = [None; MEMORY_ROW_WIDTH];
            for (offset, cell) in cells.iter_mut().enumerate() {
                *cell = island.bytes.get(&(row_start + offset as u64)).copied();
            }
            let row_end = row_start + width;
            let labels = island
                .labels
                .iter()
                .filter(|(addr, _)| *addr >= row_start && *addr < row_end)
                .map(|(_, name)| name.clone())
                .collect();
            rows.push(MemoryRow::Data {
                segment: island.segment,
                addr: row_start,
                cells,
                labels,
            });
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
                && marker_pc < addr + MEMORY_ROW_WIDTH as u64
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
            if byte_addr < *addr || byte_addr >= addr + MEMORY_ROW_WIDTH as u64 {
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
        // Mirrors Control::marker_pc: while halted, the real last
        // instruction sat 4 bytes before get_pc(). The memory pane must
        // flag that instruction, not the (past-the-end) raw PC.
        let run = MemoryRun {
            segment: Segment::Text,
            start: 0x100,
            bytes: vec![0; 16],
            labels: Vec::new(),
        };
        let rows = memory_rows(&[run]);
        let row = &rows[0];

        let raw_pc_after_halt = 0x110; // past this run entirely
        let halted_marker_pc = raw_pc_after_halt - 4; // the real last instruction

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
