//! The Realtek loader against a scripted controller.

use std::collections::{BTreeMap, VecDeque};
use std::string::String;
use std::vec::Vec;

use super::realtek_epatch::{sub, v1, v2};
use super::{cc, drive, Fake, Files, Run};
use crate::step::{Config, Done, Failure, Input, Reason, Step};
use crate::{Loader, Vendor};

const READ_LOCAL_VERSION: u16 = 0x1001;
const DOWNLOAD: u16 = 0xfc20;
const READ_REG16: u16 = 0xfc61;
const DROP_FIRMWARE: u16 = 0xfc66;
const ROM_VERSION: u16 = 0xfc6d;

/// A `Read_Local_Version` answer for (hci_ver, hci_rev, lmp_subversion).
fn version(hci_ver: u8, rev: u16, sub: u16) -> Vec<u8> {
    let mut out = std::vec![0, hci_ver];
    out.extend_from_slice(&rev.to_le_bytes());
    out.push(hci_ver);
    out.extend_from_slice(&93u16.to_le_bytes());
    out.extend_from_slice(&sub.to_le_bytes());
    out
}

const RTL8761BU: (u8, u16, u16) = (0xa, 0xb, 0x8761);
const RTL8822CU: (u8, u16, u16) = (0xa, 0xc, 0x8822);

struct FakeRtl {
    /// Answers to successive `Read_Local_Version`; the last repeats.
    versions: VecDeque<Vec<u8>>,
    rom: u8,
    key: u8,
    refuse_fragment: Option<(usize, u8)>,
    short_answer: bool,
    /// The nth (1-based) Read_Local_Version gets no answer.
    lose_read: Option<usize>,
    reads: usize,
    fragments: Vec<(u8, Vec<u8>)>,
}

impl FakeRtl {
    fn new(chip: (u8, u16, u16)) -> FakeRtl {
        FakeRtl {
            versions: [version(chip.0, chip.1, chip.2)].into(),
            rom: 0,
            key: 0,
            refuse_fragment: None,
            short_answer: false,
            lose_read: None,
            reads: 0,
            fragments: Vec::new(),
        }
    }

    fn payload(&self) -> Vec<u8> {
        self.fragments
            .iter()
            .flat_map(|(_, d)| d.iter().copied())
            .collect()
    }
}

impl Fake for FakeRtl {
    fn respond(&mut self, opcode: u16, params: &[u8]) -> Vec<Vec<u8>> {
        match opcode {
            READ_LOCAL_VERSION => {
                self.reads += 1;
                if self.lose_read == Some(self.reads) {
                    return Vec::new();
                }
                let answer = if self.versions.len() > 1 {
                    self.versions.pop_front().unwrap()
                } else {
                    self.versions[0].clone()
                };
                std::vec![cc(opcode, &answer)]
            }
            DROP_FIRMWARE => Vec::new(),
            ROM_VERSION => std::vec![cc(opcode, &[0, self.rom])],
            READ_REG16 => std::vec![cc(opcode, &[0, self.key, 0])],
            DOWNLOAD => {
                let n = self.fragments.len();
                self.fragments.push((params[0], params[1..].to_vec()));
                match self.refuse_fragment {
                    Some((at, status)) if at == n => std::vec![cc(opcode, &[status])],
                    _ if self.short_answer => std::vec![cc(opcode, &[0])],
                    _ => std::vec![cc(opcode, &[0, params[0]])],
                }
            }
            _ => std::vec![cc(opcode, &[1])],
        }
    }
}

fn files(entries: &[(&str, Vec<u8>)]) -> Files {
    entries
        .iter()
        .map(|(n, d)| (String::from(*n), d.clone()))
        .collect::<BTreeMap<_, _>>()
}

fn play(fake: &mut FakeRtl, files: &Files) -> (Loader, Run) {
    let mut loader = Loader::new(Vendor::Realtek);
    let run = drive(&mut loader, fake, files, 0, Input::Start);
    (loader, run)
}

const CONFIG: [u8; 6] = [0x55, 0xab, 0x23, 0x87, 0x00, 0x00];

fn operational() -> Step {
    Step::Done(Done::Operational {
        downloaded: true,
        config: Config::NotUsed,
    })
}

#[test]
fn an_rtl8761bu_gets_its_rom_patch_and_config_in_numbered_fragments() {
    let patch0: Vec<u8> = (0..700u32).map(|i| i as u8).collect();
    let patch1: Vec<u8> = (0..300u32).map(|i| (i * 7) as u8).collect();
    let fw = v1(&[(1, patch0), (2, patch1.clone())], 0x0102_0304, 3);
    let mut fake = FakeRtl::new(RTL8761BU);
    fake.rom = 1; // a ROM revision whose patch is the second one
    let files = files(&[
        ("rtl_bt/rtl8761bu_fw.bin", fw),
        ("rtl_bt/rtl8761bu_config.bin", CONFIG.to_vec()),
    ]);
    let (_, run) = play(&mut fake, &files);

    assert_eq!(
        run.files,
        [
            ("rtl_bt/rtl8761bu_fw.bin".into(), true),
            ("rtl_bt/rtl8761bu_config.bin".into(), false)
        ]
    );
    let mut want = patch1;
    want.extend_from_slice(&0x0102_0304u32.to_le_bytes());
    want.extend_from_slice(&CONFIG);
    assert_eq!(fake.payload(), want);
    // 310 octets: one full fragment and a last of 58 with the end flag.
    let indexes: Vec<u8> = fake.fragments.iter().map(|f| f.0).collect();
    assert_eq!(indexes, [0, 0x81]);
    assert_eq!(fake.fragments[0].1.len(), 252);
    assert_eq!(
        run.opcodes(),
        [
            READ_LOCAL_VERSION,
            ROM_VERSION,
            READ_REG16,
            DOWNLOAD,
            DOWNLOAD,
            READ_LOCAL_VERSION
        ]
    );
    assert_eq!(run.params(READ_REG16)[0], [0x10, 0xa4, 0xad, 0x00, 0xb0]);
    assert_eq!(run.last, operational());
}

#[test]
fn a_patch_that_fills_whole_fragments_ends_with_an_empty_flagged_one() {
    // 494 + 4 (version) + 6 (config) = 504 = 2 x 252.
    let fw = v1(&[(1, std::vec![0x33; 494])], 9, 3);
    let mut fake = FakeRtl::new(RTL8761BU);
    let files = files(&[
        ("rtl_bt/rtl8761bu_fw.bin", fw),
        ("rtl_bt/rtl8761bu_config.bin", CONFIG.to_vec()),
    ]);
    let (_, run) = play(&mut fake, &files);
    let shape: Vec<(u8, usize)> = fake.fragments.iter().map(|(i, d)| (*i, d.len())).collect();
    assert_eq!(shape, [(0, 252), (1, 252), (0x82, 0)]);
    assert_eq!(run.last, operational());
}

#[test]
fn the_fragment_index_wraps_after_0x7f_and_never_returns_to_zero() {
    let fw = v1(&[(1, std::vec![0x44; 130 * 252 + 10])], 9, 3);
    let mut fake = FakeRtl::new(RTL8761BU);
    let files = files(&[("rtl_bt/rtl8761bu_fw.bin", fw)]);
    let (_, run) = play(&mut fake, &files);
    let idx: Vec<u8> = fake.fragments.iter().map(|f| f.0).collect();
    assert_eq!(idx[0], 0);
    assert_eq!(idx[127], 0x7f);
    assert_eq!(idx[128], 1, "after 0x7f comes 1");
    assert_eq!(idx[129], 2);
    assert_eq!(*idx.last().unwrap() & 0x80, 0x80);
    assert_eq!(idx.iter().filter(|i| **i & 0x80 != 0).count(), 1);
    assert_eq!(run.last, operational());
}

#[test]
fn an_rtl8822cu_uses_the_v2_container_and_skips_the_missing_config() {
    let fw = v2(
        &[
            (1, std::vec![sub(1, 1, 0, b"[patch]")]),
            (3, std::vec![sub(1, 2, 5, b"[signed]")]),
        ],
        13,
    );
    let mut fake = FakeRtl::new(RTL8822CU);
    let files = files(&[("rtl_bt/rtl8822cu_fw.bin", fw)]);
    let (_, run) = play(&mut fake, &files);
    assert_eq!(
        run.files,
        [
            ("rtl_bt/rtl8822cu_fw.bin".into(), true),
            ("rtl_bt/rtl8822cu_config.bin".into(), false)
        ]
    );
    assert_eq!(fake.payload(), b"[patch]", "key 0 ignores signed sections");
    assert_eq!(run.last, operational());
}

#[test]
fn a_key_protected_controller_gets_signed_sections_and_no_config() {
    let fw = v2(&[(3, std::vec![sub(1, 2, 5, b"[signed]")])], 13);
    let mut fake = FakeRtl::new(RTL8822CU);
    fake.key = 5;
    let files = files(&[("rtl_bt/rtl8822cu_fw.bin", fw)]);
    let (_, run) = play(&mut fake, &files);
    assert_eq!(run.files.len(), 1, "no config is asked for");
    assert_eq!(fake.payload(), b"[signed]");
}

#[test]
fn the_rtl8852c_prefers_its_v2_image_and_falls_back() {
    let rtl8852c = (0xc, 0xc, 0x8852);
    let patch = v2(&[(1, std::vec![sub(1, 1, 0, b"[52c]")])], 25);
    let mut fake = FakeRtl::new(rtl8852c);
    let (_, run) = play(
        &mut fake,
        &files(&[("rtl_bt/rtl8852cu_fw.bin", patch.clone())]),
    );
    assert_eq!(
        run.files[..2],
        [
            ("rtl_bt/rtl8852cu_fw_v2.bin".into(), false),
            ("rtl_bt/rtl8852cu_fw.bin".into(), true)
        ]
    );
    assert_eq!(fake.payload(), b"[52c]");
    let mut fake = FakeRtl::new(rtl8852c);
    let (_, run) = play(&mut fake, &files(&[("rtl_bt/rtl8852cu_fw_v2.bin", patch)]));
    assert_eq!(run.files[0].0, "rtl_bt/rtl8852cu_fw_v2.bin");
    assert_eq!(run.last, operational());
}

#[test]
fn the_oldest_rom_takes_a_plain_image_and_no_rom_version() {
    let mut fake = FakeRtl::new((6, 0xb, 0x1200));
    let image: Vec<u8> = (0..500u32).map(|i| i as u8).collect();
    let (_, run) = play(
        &mut fake,
        &files(&[("rtl_bt/rtl8723a_fw.bin", image.clone())]),
    );
    assert_eq!(run.count(ROM_VERSION), 0);
    assert_eq!(run.files.len(), 1, "no config file for this ROM");
    assert_eq!(fake.payload(), image);
    // An epatch where a plain image belongs is refused.
    let mut fake = FakeRtl::new((6, 0xb, 0x1200));
    let (_, run) = play(
        &mut fake,
        &files(&[("rtl_bt/rtl8723a_fw.bin", v1(&[(1, std::vec![1])], 1, 0))]),
    );
    assert!(matches!(
        run.last,
        Step::Failed(Failure {
            reason: Reason::BadFirmware(_),
            ..
        })
    ));
    assert!(fake.fragments.is_empty());
}

#[test]
fn a_required_config_that_is_missing_stops_the_load() {
    let mut fake = FakeRtl::new((8, 0xd, 0x8723)); // RTL8723D
    let patch = v1(&[(1, std::vec![1; 10])], 1, 9);
    let (_, run) = play(&mut fake, &files(&[("rtl_bt/rtl8723d_fw.bin", patch)]));
    assert_eq!(run.files[1], ("rtl_bt/rtl8723d_config.bin".into(), true));
    assert_eq!(
        run.last,
        Step::Failed(Failure {
            reason: Reason::FileMissing,
            reenumerate: false
        })
    );
    assert!(fake.fragments.is_empty());
}

#[test]
fn an_unknown_controller_is_asked_to_drop_its_stock_image_once() {
    // Stock image answers unknown values, then the ROM reports itself.
    let mut fake = FakeRtl::new((9, 0x1234, 0x4321));
    fake.versions
        .push_back(version(RTL8761BU.0, RTL8761BU.1, RTL8761BU.2));
    let fw = v1(&[(1, std::vec![7; 20])], 1, 3);
    let files = files(&[("rtl_bt/rtl8761bu_fw.bin", fw)]);
    let (mut loader, run) = play(&mut fake, &files);
    assert_eq!(run.opcodes(), [READ_LOCAL_VERSION, DROP_FIRMWARE]);
    let t = loader.deadline().expect("200 ms wait");
    assert_eq!(t, 200);
    assert_eq!(loader.step(t - 1, Input::Tick), Step::Wait);
    let run = drive(&mut loader, &mut fake, &files, t, Input::Tick);
    assert_eq!(run.opcodes()[0], READ_LOCAL_VERSION);
    assert_eq!(run.last, operational());
}

#[test]
fn a_controller_still_unknown_after_the_drop_needs_no_firmware() {
    let mut fake = FakeRtl::new((9, 0x1234, 0x4321));
    let (mut loader, _) = play(&mut fake, &Files::new());
    let run = drive(&mut loader, &mut fake, &Files::new(), 200, Input::Tick);
    assert_eq!(run.last, Step::Done(Done::NoFirmwareNeeded));
    assert_eq!(run.count(DROP_FIRMWARE), 0, "only once");
}

#[test]
fn a_refused_fragment_fails_the_load_without_sending_more() {
    let fw = v1(&[(1, std::vec![5; 2000])], 1, 3);
    let mut fake = FakeRtl::new(RTL8761BU);
    fake.refuse_fragment = Some((2, 0x12));
    let (_, run) = play(&mut fake, &files(&[("rtl_bt/rtl8761bu_fw.bin", fw)]));
    assert_eq!(run.count(DOWNLOAD), 3);
    assert_eq!(
        run.last,
        Step::Failed(Failure {
            reason: Reason::CommandFailed {
                opcode: DOWNLOAD,
                status: 0x12
            },
            reenumerate: false
        })
    );
}

#[test]
fn a_fragment_answer_of_the_wrong_size_fails_the_load() {
    let fw = v1(&[(1, std::vec![5; 2000])], 1, 3);
    let mut fake = FakeRtl::new(RTL8761BU);
    fake.short_answer = true;
    let (_, run) = play(&mut fake, &files(&[("rtl_bt/rtl8761bu_fw.bin", fw)]));
    assert_eq!(run.count(DOWNLOAD), 1);
    assert!(matches!(
        run.last,
        Step::Failed(Failure {
            reason: Reason::BadResponse { opcode: DOWNLOAD },
            ..
        })
    ));
}

#[test]
fn files_that_do_not_fit_the_chip_are_refused_before_anything_is_sent() {
    let wrong_chip = v1(&[(1, std::vec![5; 20])], 1, 13); // an RTL8822C patch
    let mut fake = FakeRtl::new(RTL8761BU);
    let (_, run) = play(
        &mut fake,
        &files(&[("rtl_bt/rtl8761bu_fw.bin", wrong_chip)]),
    );
    assert!(matches!(
        run.last,
        Step::Failed(Failure {
            reason: Reason::BadFirmware(_),
            ..
        })
    ));
    let mut fake = FakeRtl::new(RTL8761BU);
    let (_, run) = play(&mut fake, &Files::new());
    assert!(matches!(
        run.last,
        Step::Failed(Failure {
            reason: Reason::FileMissing,
            ..
        })
    ));
    assert_eq!(run.count(DOWNLOAD), 0);
}

#[test]
fn silence_times_out_and_a_detach_is_reported() {
    let mut loader = Loader::new(Vendor::Realtek);
    assert!(matches!(loader.step(10, Input::Start), Step::Send(_)));
    let t = loader.deadline().unwrap();
    assert!(matches!(
        loader.step(t, Input::Tick),
        Step::Failed(Failure {
            reason: Reason::Timeout {
                opcode: READ_LOCAL_VERSION
            },
            ..
        })
    ));
    let mut loader = Loader::new(Vendor::Realtek);
    let _ = loader.step(0, Input::Start);
    assert!(matches!(
        loader.step(1, Input::Detached),
        Step::Failed(Failure {
            reason: Reason::Detached,
            ..
        })
    ));
}

#[test]
fn a_device_that_leaves_after_the_last_fragment_has_restarted_on_the_new_image() {
    let fw = v1(&[(1, std::vec![5; 20])], 1, 3);
    let mut fake = FakeRtl::new(RTL8761BU);
    let files = files(&[("rtl_bt/rtl8761bu_fw.bin", fw)]);
    fake.lose_read = Some(2); // the version read that proves the new image
    let (mut loader, run) = play(&mut fake, &files);
    assert_eq!(run.last, Step::Wait);
    assert_eq!(
        loader.step(1, Input::Detached),
        Step::Done(Done::Reenumerate)
    );
}
