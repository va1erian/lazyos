//! What each process's memory is made of: the client of the kernel's
//! per-process memory block (system-stats op 5, `kernel/src/sysinfo/procmem.rs`;
//! the header and row constants below mirror it).
//!
//! A page that several processes map is divided between them, so
//! [`ProcessMemory::pss`] adds up across processes to the memory the programs
//! hold, and a binary's text is counted once however many processes run it.
//! [`ProcessMemory::private`] is what freeing the process alone would give
//! back. The block carries sizes only, never an address, so every task may
//! read it.

use alloc::vec;
use alloc::vec::Vec;

use super::memory::PAGE;
use crate::stats;

/// ABI version of the block.
pub const VERSION: u64 = 1;
/// Words in the header (mirrors `kernel::sysinfo::procmem::HEADER_WORDS`).
pub const HEADER_WORDS: usize = 5;
/// What a page is for: the classes of [`ProcessMemory::class_pss`].
pub const CLASSES: usize = 5;
/// Words in one row (mirrors `kernel::sysinfo::procmem::ROW_WORDS`).
pub const ROW_WORDS: usize = 7 + 2 * CLASSES;
/// Rows asked for: one per scheduler slot at most.
pub const MAX_ROWS: usize = super::MAX_TASKS;

/// What a resident page is for, in the order of the class arrays.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// Read-only program segments, shared by every process running the file.
    Image,
    /// Heap, anonymous and written data.
    Private,
    /// Stacks.
    Stack,
    /// Memory shared on purpose: Messenger buffers and shared mappings.
    Shared,
    /// Private file mappings (fonts, data files).
    Mapped,
}

impl Class {
    /// Every class, in array order.
    pub const ALL: [Class; CLASSES] = [
        Class::Image,
        Class::Private,
        Class::Stack,
        Class::Shared,
        Class::Mapped,
    ];

    /// A short name for tables.
    pub fn label(self) -> &'static str {
        match self {
            Class::Image => "program code",
            Class::Private => "heap and data",
            Class::Stack => "stacks",
            Class::Shared => "shared buffers",
            Class::Mapped => "mapped files",
        }
    }
}

/// One process (a user address space; its threads count once).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ProcessMemory {
    /// The process: its thread-group leader's slot, what `getpid` says in
    /// every thread.
    pub pid: u64,
    /// Tasks (threads) running in the space.
    pub threads: u64,
    /// Resident pages, each counted in full.
    pub resident_pages: u64,
    /// Resident pages no other process maps (USS).
    pub private_pages: u64,
    /// Proportional bytes: every page divided by the processes mapping it.
    pub pss: u64,
    /// Page-table frames.
    pub table_pages: u64,
    /// Address space reserved, in bytes (most of it never touched).
    pub mapped: u64,
    /// Resident pages by [`Class`].
    pub class_pages: [u64; CLASSES],
    /// Proportional bytes by [`Class`]; they add up to [`ProcessMemory::pss`].
    pub class_pss: [u64; CLASSES],
}

impl ProcessMemory {
    /// Resident bytes, shared pages counted in full (RSS).
    pub fn resident(&self) -> u64 {
        self.resident_pages * PAGE
    }

    /// Bytes only this process holds (USS), page tables included.
    pub fn private(&self) -> u64 {
        (self.private_pages + self.table_pages) * PAGE
    }

    /// What the process costs the machine: its proportional share of the pages
    /// it maps plus its page tables. The figure to rank processes by.
    pub fn footprint(&self) -> u64 {
        self.pss + self.table_pages * PAGE
    }

    /// Bytes shared with other processes, counted in full.
    pub fn shared(&self) -> u64 {
        (self.resident_pages - self.private_pages.min(self.resident_pages)) * PAGE
    }
}

/// The decoded block.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ProcessTable {
    /// PIT ticks at the time of the read.
    pub ticks: u64,
    /// Processes that exist; `rows.len()` may be fewer if the buffer was short.
    pub total: u64,
    /// One row per process, in slot order.
    pub rows: Vec<ProcessMemory>,
}

impl ProcessTable {
    /// The row of the process `pid`.
    pub fn of(&self, pid: u64) -> Option<&ProcessMemory> {
        self.rows.iter().find(|row| row.pid == pid)
    }

    /// The footprints of all processes added up: the memory the programs hold.
    pub fn footprint(&self) -> u64 {
        self.rows.iter().map(ProcessMemory::footprint).sum()
    }

    /// The resident sizes added up, shared pages counted once in every process
    /// that maps them: what adding up the usual per-process figure says. The
    /// gap to [`ProcessTable::footprint`] is the sharing.
    pub fn resident(&self) -> u64 {
        self.rows.iter().map(ProcessMemory::resident).sum()
    }
}

/// Decode a block from raw words. `None` for a version this client does not
/// know, a different row size or a block shorter than its header says.
pub fn decode_words(words: &[u64]) -> Option<ProcessTable> {
    if words.len() < HEADER_WORDS || words[0] != VERSION || words[1] != ROW_WORDS as u64 {
        return None;
    }
    let written = usize::try_from(words[2]).ok()?;
    let (body, _) = words[HEADER_WORDS..].as_chunks::<ROW_WORDS>();
    if body.len() < written {
        return None;
    }
    let rows = body[..written].iter().map(decode_row).collect();
    Some(ProcessTable {
        ticks: words[4],
        total: words[3],
        rows,
    })
}

fn decode_row(row: &[u64; ROW_WORDS]) -> ProcessMemory {
    let mut class_pages = [0; CLASSES];
    let mut class_pss = [0; CLASSES];
    class_pages.copy_from_slice(&row[7..7 + CLASSES]);
    class_pss.copy_from_slice(&row[7 + CLASSES..7 + 2 * CLASSES]);
    ProcessMemory {
        pid: row[0],
        threads: row[1],
        resident_pages: row[2],
        private_pages: row[3],
        pss: row[4],
        table_pages: row[5],
        mapped: row[6],
        class_pages,
        class_pss,
    }
}

/// Decode a block from little-endian bytes.
pub fn decode_bytes(bytes: &[u8]) -> Option<ProcessTable> {
    let (chunks, _) = bytes.as_chunks::<8>();
    let words: Vec<u64> = chunks
        .iter()
        .map(|chunk| u64::from_le_bytes(*chunk))
        .collect();
    decode_words(&words)
}

/// Read every process's memory through the native syscall. Returns the
/// negative errno on failure (`-EINVAL` when the kernel's block is not the
/// version this client reads).
pub fn read() -> Result<ProcessTable, i64> {
    let mut bytes = vec![0u8; (HEADER_WORDS + MAX_ROWS * ROW_WORDS) * 8];
    let written = stats::process_memory(&mut bytes)?;
    bytes.truncate(written);
    decode_bytes(&bytes).ok_or(-crate::errno::EINVAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u64, pss: u64) -> Vec<u64> {
        let mut row = vec![pid, 1, 10, 4, pss, 2, 1 << 20];
        row.extend([1, 2, 3, 2, 2]);
        row.extend([100, 200, 300, 400, 500]);
        row
    }

    fn block(rows: &[Vec<u64>], total: u64) -> Vec<u64> {
        let mut words = vec![VERSION, ROW_WORDS as u64, rows.len() as u64, total, 77];
        for row in rows {
            words.extend(row);
        }
        words
    }

    #[test]
    fn a_block_decodes_into_rows() {
        let table = decode_words(&block(&[row(3, 9000), row(5, 100)], 2)).expect("decodes");
        assert_eq!((table.ticks, table.total, table.rows.len()), (77, 2, 2));
        let first = table.of(3).expect("pid 3");
        assert_eq!(
            (first.threads, first.resident_pages, first.pss),
            (1, 10, 9000)
        );
        assert_eq!(first.class_pages, [1, 2, 3, 2, 2]);
        assert_eq!(first.class_pss, [100, 200, 300, 400, 500]);
        assert_eq!(first.resident(), 10 * PAGE);
        assert_eq!(first.private(), (4 + 2) * PAGE);
        assert_eq!(first.shared(), 6 * PAGE);
        assert_eq!(first.footprint(), 9000 + 2 * PAGE);
        assert_eq!(table.footprint(), 9000 + 100 + 4 * PAGE);
        assert!(table.of(4).is_none());
    }

    #[test]
    fn a_short_buffer_keeps_the_total() {
        let table = decode_words(&block(&[row(1, 1)], 40)).expect("decodes");
        assert_eq!((table.rows.len(), table.total), (1, 40));
    }

    #[test]
    fn a_block_it_cannot_read_is_refused() {
        let mut words = block(&[row(1, 1)], 1);
        words[0] = VERSION + 1;
        assert!(decode_words(&words).is_none(), "another version");
        let mut words = block(&[row(1, 1)], 1);
        words[1] += 1;
        assert!(decode_words(&words).is_none(), "another row size");
        let mut words = block(&[row(1, 1)], 1);
        words.pop();
        assert!(decode_words(&words).is_none(), "a row cut short");
        assert!(decode_words(&words[..3]).is_none(), "a cut header");
    }

    #[test]
    fn bytes_decode_like_words() {
        let words = block(&[row(2, 4096)], 1);
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(decode_bytes(&bytes), decode_words(&words));
    }

    #[test]
    fn a_private_count_above_the_resident_one_does_not_wrap() {
        let row = ProcessMemory {
            resident_pages: 1,
            private_pages: 3,
            ..ProcessMemory::default()
        };
        assert_eq!(row.shared(), 0);
    }
}
