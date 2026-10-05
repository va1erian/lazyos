//! Host tests: bring-up and I/O against the model controller, PRP planning,
//! Identify parsing, entry layouts, and the seeded fuzz entry point.

use std::collections::HashMap;
use std::vec;
use std::vec::Vec;

use crate::cmd::{Command, Completion};
use crate::identify::{ControllerInfo, Namespace, NamespaceError};
use crate::prp::{self, Cursor, PlanError};
use crate::regs::Cap;
use crate::{Controller, Platform, PAGE};

mod io;
mod model;
mod prp_cases;

use model::{Behavior, Model};

/// A buffer at a pretend virtual address whose pages map to scattered
/// model pages.
pub(super) struct Buffer {
    pub(super) virt: u64,
    pub(super) len: usize,
    map: HashMap<u64, u64>,
}

impl Buffer {
    /// `len` bytes starting `offset` bytes into a fresh virtual page.
    pub(super) fn new(model: &Model, base: u64, offset: u64, len: usize) -> Buffer {
        let virt = base + offset;
        let mut map = HashMap::new();
        let mut page = base;
        while page < virt + len as u64 {
            map.insert(page, model.alloc_page());
            page += PAGE;
        }
        Buffer { virt, len, map }
    }

    pub(super) fn translate(&self, virt: u64) -> Option<u64> {
        let page = virt & !(PAGE - 1);
        self.map.get(&page).map(|phys| phys + virt % PAGE)
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

pub(super) fn translate_all<'a>(buffers: &[&'a Buffer]) -> impl Fn(u64) -> Option<u64> + 'a {
    let buffers: Vec<&'a Buffer> = buffers.to_vec();
    move |virt| buffers.iter().find_map(|buffer| buffer.translate(virt))
}

/// A wait that lets the model finish deferred commands, then polls a while.
pub(super) fn wait_for(model: &Model) -> impl FnMut(&dyn Fn() -> bool) -> bool + '_ {
    move |ready| {
        for _ in 0..100 {
            model.run_pending();
            if ready() {
                return true;
            }
        }
        false
    }
}

pub(super) fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

pub(super) fn up(model: &Model) -> Controller {
    Controller::init(model, model.pages()).expect("bring-up")
}

#[test]
fn cap_decodes_fields() {
    let cap = Cap::decode(1023 | 1 << 16 | 20 << 24 | 2u64 << 32 | 1 << 37 | 1 << 48 | 4 << 52);
    assert_eq!(cap.mqes, 1024);
    assert!(cap.cqr && cap.nvm);
    assert_eq!(cap.timeout_ms, 10_000);
    assert_eq!(cap.doorbell_stride, 16);
    assert_eq!((cap.page_min_shift, cap.page_max_shift), (13, 16));
    assert_eq!(Cap::decode(0).timeout_ms, 500);
}

#[test]
fn entries_round_trip() {
    let command = Command::io(crate::cmd::nvm::WRITE, 1, 0x1_2345_6789, 8, 0x1000, 0x2000);
    let command = Command {
        cid: 0x1234,
        ..command
    };
    assert_eq!(Command::decode(&command.encode()), command);
    let completion = Completion {
        result: 7,
        sq_head: 3,
        sq_id: 1,
        cid: 0xBEEF,
        phase: true,
        sct: 2,
        sc: 0x81,
        dnr: true,
    };
    assert_eq!(Completion::decode(&completion.encode()), completion);
    assert!(!completion.ok());
}

#[test]
fn identify_namespace_refusals() {
    let behavior = Behavior::default();
    let good = model::identify_namespace(&behavior, 1 << 20);
    assert_eq!(Namespace::parse(&good).unwrap().blocks, 2048);
    let mut empty = good.clone();
    empty[..8].fill(0);
    assert_eq!(Namespace::parse(&empty), Err(NamespaceError::Empty));
    let mut format = good.clone();
    format[26] = 3;
    assert_eq!(Namespace::parse(&format), Err(NamespaceError::BadFormat));
    let mut metadata = good.clone();
    metadata[128] = 8;
    assert_eq!(Namespace::parse(&metadata), Err(NamespaceError::Metadata));
    let mut size = good.clone();
    size[130] = 30;
    assert_eq!(Namespace::parse(&size), Err(NamespaceError::BadBlockSize));
    let mut cap = good.clone();
    cap[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(Namespace::parse(&cap), Err(NamespaceError::Inconsistent));
    assert_eq!(Namespace::parse(&good[..100]), Err(NamespaceError::Empty));
}

#[test]
fn identify_controller_text_and_mdts() {
    let mut page = model::identify_controller(&Behavior {
        mdts: 5,
        ..Behavior::default()
    });
    page[30] = 0xFF;
    let info = ControllerInfo::parse(&page).unwrap();
    assert_eq!(info.model.as_str(), "LazyOS?model NVMe controller");
    assert_eq!(info.max_transfer(12), Some(128 * 1024));
    let huge = ControllerInfo { mdts: 255, ..info };
    assert_eq!(huge.max_transfer(12), None);
    assert!(ControllerInfo::parse(&page[..10]).is_none());
}

#[test]
fn plan_errors() {
    let identity = |virt: u64| Some(virt);
    assert_eq!(
        prp::plan(&[(0x1001, 512)], Cursor::default(), 4096, 512, &identity),
        Err(PlanError::Misaligned)
    );
    assert_eq!(
        prp::plan(&[(0x1000, 512)], Cursor::default(), 4096, 512, &|_| None),
        Err(PlanError::Unmapped)
    );
    // 256 bytes then a break: less than one block.
    assert_eq!(
        prp::plan(
            &[(0x1E00, 256), (0x5000, 256)],
            Cursor::default(),
            4096,
            512,
            &identity
        ),
        Err(PlanError::Misaligned)
    );
}

#[test]
fn seeded_fuzz() {
    fuzzkit::for_seeds("nvme_fuzz", |_, rng| {
        let len = rng.range(0, 600) as usize;
        let data = rng.bytes(len);
        crate::fuzz::run(&data);
    });
    // Whole Identify pages, mostly valid, with bits flipped.
    fuzzkit::for_seeds("nvme_fuzz_identify", |_, rng| {
        let mut input = vec![rng.byte() & 1];
        let mut page = if input[0] == 0 {
            model::identify_controller(&Behavior::default())
        } else {
            model::identify_namespace(&Behavior::default(), 1 << 24)
        };
        let flips = rng.range(1, 16) as usize;
        rng.flip_bits(&mut page[..256], flips);
        input.extend_from_slice(&page);
        crate::fuzz::run(&input);
    });
}

#[test]
fn checked_in_fuzz_seeds_run_clean() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/nvme");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return; // a checkout without the fuzz corpus
    };
    for entry in entries.flatten() {
        let data = std::fs::read(entry.path()).unwrap();
        crate::fuzz::run(&data);
    }
}
