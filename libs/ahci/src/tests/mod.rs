//! Host tests: bring-up and I/O against the model HBA, planning, IDENTIFY
//! parsing, structure layouts and the seeded fuzz entry point.

use std::boxed::Box;
use std::collections::HashMap;
use std::vec;
use std::vec::Vec;

use crate::{Hba, Platform, Port, Skip};

mod io;
mod layout;
mod model;
mod open;
mod plan;

use model::{Behavior, Model, BUFFER_BASE};

/// A buffer at a pretend virtual address whose pages map to scattered model
/// pages.
pub(super) struct Buffer {
    pub(super) virt: u64,
    pub(super) len: usize,
    map: HashMap<u64, u64>,
}

impl Buffer {
    /// `len` bytes starting `offset` bytes into a fresh virtual page; the
    /// pages map to model pages in reverse order, `first` pages in.
    pub(super) fn new(base: u64, offset: u64, len: usize, first: u64) -> Buffer {
        let virt = base + offset;
        let pages = (offset as usize + len).div_ceil(4096) as u64;
        let map = (0..pages)
            .map(|index| {
                (
                    base + index * 4096,
                    BUFFER_BASE + (first + pages - 1 - index) * 4096,
                )
            })
            .collect();
        Buffer { virt, len, map }
    }

    pub(super) fn translate(&self, virt: u64) -> Option<u64> {
        self.map.get(&(virt & !4095)).map(|phys| phys + virt % 4096)
    }

    pub(super) fn fill(&self, model: &Model, data: &[u8]) {
        for (index, &byte) in data.iter().enumerate() {
            let phys = self.translate(self.virt + index as u64).unwrap();
            model.write_mem(phys, &[byte]);
        }
    }

    pub(super) fn contents(&self, model: &Model) -> Vec<u8> {
        (0..self.len)
            .map(|index| {
                let mut byte = [0u8];
                model.read_mem(self.translate(self.virt + index as u64).unwrap(), &mut byte);
                byte[0]
            })
            .collect()
    }
}

/// A caller's wait: polls the readiness test, says whether it held.
pub(super) type Waiter<'a> = Box<dyn FnMut(&dyn Fn() -> bool) -> bool + 'a>;

/// A wait that polls `ready` up to `polls` times.
pub(super) fn waiter(model: &Model, polls: usize) -> Waiter<'_> {
    Box::new(move |ready| {
        for _ in 0..polls {
            if ready() {
                return true;
            }
            model.relax();
        }
        false
    })
}

/// Bring up the HBA and port 0.
pub(super) fn open(model: &Model) -> Result<Port, Skip> {
    let hba = Hba::init(model).expect("hba");
    hba.open_port(model, 0, model.pages())
}

pub(super) fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|index| {
            (index as u8)
                .wrapping_mul(31)
                .wrapping_add(seed)
                .wrapping_add((index >> 8) as u8)
        })
        .collect()
}

pub(super) fn behavior() -> Behavior {
    Behavior::default()
}

#[test]
fn seeded_fuzz() {
    fuzzkit::for_seeds("ahci_fuzz", |_, rng| {
        let len = rng.range(0, 600) as usize;
        let data = rng.bytes(len);
        crate::fuzz::run(&data);
    });
    // Whole IDENTIFY pages, mostly valid, with bits flipped.
    fuzzkit::for_seeds("ahci_fuzz_identify", |_, rng| {
        let model = Model::new(1 << 20);
        open(&model).expect("a plain disk opens");
        // `pages()` hands out the IDENTIFY page last.
        let mut page = [0u8; 512];
        model.read_mem(model.last_page(), &mut page);
        let flips = rng.range(1, 16) as usize;
        rng.flip_bits(&mut page, flips);
        let mut input = vec![0u8];
        input.extend_from_slice(&page);
        crate::fuzz::run(&input);
    });
}

#[test]
fn checked_in_fuzz_seeds_run_clean() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/ahci");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return; // a checkout without the fuzz corpus
    };
    for entry in entries.flatten() {
        let data = std::fs::read(entry.path()).unwrap();
        crate::fuzz::run(&data);
    }
}
