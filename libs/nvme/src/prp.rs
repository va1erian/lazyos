//! Cutting a transfer into commands whose data pointers obey the PRP rules
//! (NVMe 1.4 section 4.3).
//!
//! The device reads and writes the caller's buffers directly. A command's
//! data is a list of Physical Region Page entries: the first may start
//! anywhere dword aligned, every later one must start on a page boundary,
//! and every entry but the last must run to the end of its page. The
//! caller's buffers are virtually contiguous runs over scattered frames
//! (the kernel heap and stacks), and a vectored transfer is several such
//! runs, so a command takes pieces while those rules hold and stops where
//! they would break: at a segment boundary that falls inside a page, at
//! [`MAX_ENTRIES`], or at the byte limit. It always ends on a block
//! boundary, so a segment may straddle two commands.
//!
//! Two pieces that are physically contiguous inside one page (two buffers
//! back to back in the same frame) merge into one entry.

use crate::PAGE;

/// PRP entries in one command: enough for 64 KiB starting mid-page.
pub const MAX_ENTRIES: usize = 17;

/// A position in a segment list: which segment, and how far into it.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub segment: usize,
    pub offset: usize,
}

impl Cursor {
    /// Move `bytes` forward through `segments` (`(address, length)` pairs).
    pub fn advance(&mut self, segments: &[(u64, usize)], mut bytes: usize) {
        while bytes > 0 {
            let Some(&(_, len)) = segments.get(self.segment) else {
                return;
            };
            let step = (len - self.offset).min(bytes);
            bytes -= step;
            self.offset += step;
            if self.offset == len {
                self.segment += 1;
                self.offset = 0;
            }
        }
        // Skip empty segments so the cursor rests on data.
        while segments.get(self.segment).is_some_and(|&(_, len)| len == 0) {
            self.segment += 1;
        }
    }
}

/// One command's data: its PRP entries (physical address, length).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub entries: [(u64, u32); MAX_ENTRIES],
    pub count: usize,
    pub bytes: usize,
}

impl Plan {
    /// `PRP1`.
    pub fn prp1(&self) -> u64 {
        self.entries[0].0
    }

    /// Whether `PRP2` must point at a PRP list (more than two entries).
    pub fn needs_list(&self) -> bool {
        self.count > 2
    }

    /// `PRP2` when no list is needed: the second entry, or zero.
    pub fn prp2_direct(&self) -> u64 {
        if self.count == 2 {
            self.entries[1].0
        } else {
            0
        }
    }

    /// The PRP list (entries 2..), little endian, for the list page.
    pub fn list(&self, out: &mut [u8; MAX_ENTRIES * 8]) -> usize {
        let mut len = 0;
        for &(phys, _) in &self.entries[1..self.count] {
            out[len..len + 8].copy_from_slice(&phys.to_le_bytes());
            len += 8;
        }
        len
    }
}

/// Why no command could be planned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// A page of the buffer has no physical address.
    Unmapped,
    /// The first piece is not dword aligned, or the pieces that fit hold
    /// less than one block.
    Misaligned,
}

/// The next command from `at`: entries that obey the PRP rules, at most
/// `max_bytes` (a `block` multiple) bytes, trimmed back to a `block`
/// boundary. `translate` maps a virtual address to its physical one (the
/// offset within the page is preserved).
pub fn plan(
    segments: &[(u64, usize)],
    at: Cursor,
    max_bytes: usize,
    block: usize,
    translate: &dyn Fn(u64) -> Option<u64>,
) -> Result<Plan, PlanError> {
    let mut plan = Plan {
        entries: [(0, 0); MAX_ENTRIES],
        count: 0,
        bytes: 0,
    };
    let mut cursor = at;
    'outer: while plan.bytes < max_bytes {
        let Some(&(base, len)) = segments.get(cursor.segment) else {
            break;
        };
        if cursor.offset >= len {
            cursor.segment += 1;
            cursor.offset = 0;
            continue;
        }
        let virt = base + cursor.offset as u64;
        let in_page = (PAGE - virt % PAGE) as usize;
        let take = in_page.min(len - cursor.offset).min(max_bytes - plan.bytes);
        let phys = translate(virt).ok_or(PlanError::Unmapped)?;
        if plan.count == 0 {
            if phys % 4 != 0 {
                return Err(PlanError::Misaligned);
            }
        } else {
            let (last, last_len) = plan.entries[plan.count - 1];
            let end = last + u64::from(last_len);
            if phys == end && end % PAGE != 0 {
                // Contiguous in the same page: grow the last entry.
                plan.entries[plan.count - 1].1 += take as u32;
                plan.bytes += take;
                cursor.offset += take;
                continue 'outer;
            }
            if end % PAGE != 0 || phys % PAGE != 0 || plan.count == MAX_ENTRIES {
                break;
            }
        }
        plan.entries[plan.count] = (phys, take as u32);
        plan.count += 1;
        plan.bytes += take;
        cursor.offset += take;
    }
    // Trim back to a block boundary, dropping entries past it.
    let keep = plan.bytes - plan.bytes % block.max(1);
    if keep == 0 {
        return Err(PlanError::Misaligned);
    }
    let mut seen = 0usize;
    let mut count = 0;
    for entry in plan.entries.iter_mut().take(plan.count) {
        if seen >= keep {
            *entry = (0, 0);
            continue;
        }
        let room = keep - seen;
        if (entry.1 as usize) > room {
            entry.1 = room as u32;
        }
        seen += entry.1 as usize;
        count += 1;
    }
    plan.count = count;
    plan.bytes = keep;
    Ok(plan)
}

/// Whether `plan` obeys the PRP rules (for tests and the fuzz entry point).
pub fn valid(plan: &Plan) -> bool {
    if plan.count == 0 || plan.count > MAX_ENTRIES {
        return false;
    }
    let entries = &plan.entries[..plan.count];
    let total: usize = entries.iter().map(|&(_, len)| len as usize).sum();
    if total != plan.bytes || !entries[0].0.is_multiple_of(4) {
        return false;
    }
    for (index, &(phys, len)) in entries.iter().enumerate() {
        if len == 0 || phys % PAGE + u64::from(len) > PAGE {
            return false;
        }
        if index > 0 && phys % PAGE != 0 {
            return false;
        }
        if index + 1 < entries.len() && (phys + u64::from(len)) % PAGE != 0 {
            return false;
        }
    }
    true
}
