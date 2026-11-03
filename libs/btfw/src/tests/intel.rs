//! The Intel loader against a scripted controller: the good paths.

use std::collections::BTreeMap;
use std::vec::Vec;

use super::{cc, drive, vendor_event, Fake, Files, Run};
use crate::step::{Config, Done, Input, Step};
use crate::{Loader, Vendor};

pub(crate) const RESET: u16 = 0xfc01;
pub(crate) const READ_VERSION: u16 = 0xfc05;
pub(crate) const SECURE_SEND: u16 = 0xfc09;
pub(crate) const BOOT_PARAMS: u16 = 0xfc0d;
pub(crate) const WRITE_DDC: u16 = 0xfc8b;

pub(crate) const BOOT_ADDR: u32 = 0x0010_0800;
pub(crate) const FW_BUILD: (u8, u8, u8) = (3, 14, 26);

fn command(opcode: u16, params: &[u8]) -> Vec<u8> {
    let mut out = opcode.to_le_bytes().to_vec();
    out.push(params.len() as u8);
    out.extend_from_slice(params);
    out
}

/// The command stream of a synthetic image, in 4-octet-aligned groups as the
/// real files are: boot parameters plus a NOP (16 octets), three big commands
/// (a 256-octet group takes two fragments: 252 and 4), a group of two small
/// ones, and a last small one.
pub(crate) fn body(build: (u8, u8, u8)) -> Vec<u8> {
    let mut params = BOOT_ADDR.to_le_bytes().to_vec();
    params.extend_from_slice(&[build.0, build.1, build.2]);
    let mut out = command(0xfc0e, &params);
    out.extend(command(0xfc8e, &[0, 0, 0]));
    for (i, n) in [253usize, 253, 249].into_iter().enumerate() {
        out.extend(command(0xfc0b, &vec![0x50 + i as u8; n]));
    }
    out.extend(command(0xfc8e, &[1, 2, 3]));
    out.extend(command(0xfc8e, &[4, 5, 6]));
    out.extend(command(0xfc8e, &[0]));
    out
}

/// A synthetic `.sfi`: the RSA block (every octet a recognizable value), the
/// ECDSA block when asked, then `body`.
pub(crate) fn sfi(ecdsa: bool, body: &[u8]) -> Vec<u8> {
    let mut file: Vec<u8> = (0..644).map(|i| (i % 251) as u8 ^ 0x40).collect();
    file[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    if ecdsa {
        let mut block: Vec<u8> = (0..320).map(|i| (i % 241) as u8 ^ 0x20).collect();
        block[0] = 0x06;
        block[8..12].copy_from_slice(&0x0002_0000u32.to_le_bytes());
        file.extend(block);
    }
    file.extend_from_slice(body);
    file
}

/// Legacy Read_Version answer (status first).
pub(crate) fn legacy_version(hw_variant: u8, fw_variant: u8, build: (u8, u8, u8)) -> Vec<u8> {
    vec![
        0, 0x37, hw_variant, 0, fw_variant, 4, build.0, build.1, build.2, 0,
    ]
}

/// Boot parameters answer: device revision and the fragment-ack flag.
pub(crate) fn boot_params(dev_revid: u16, limited_cce: u8) -> Vec<u8> {
    let mut out = vec![0u8; 23];
    out[4..6].copy_from_slice(&dev_revid.to_le_bytes());
    out[21] = limited_cce;
    out
}

fn tlv(out: &mut Vec<u8>, ty: u8, val: &[u8]) {
    out.push(ty);
    out.push(val.len() as u8);
    out.extend_from_slice(val);
}

/// TLV Read_Version answer. `cnvi_top`/`cnvr_top` are the CNVx words
/// (type in the low 12 bits, step in bits 24..28).
pub(crate) fn tlv_version(hw_variant: u8, img_type: u8, sbe: u8, tops: (u32, u32)) -> Vec<u8> {
    let mut out = vec![0u8];
    tlv(&mut out, 0x10, &tops.0.to_le_bytes());
    tlv(&mut out, 0x11, &tops.1.to_le_bytes());
    let cnvi_bt = 0x37u32 << 8 | u32::from(hw_variant) << 16;
    tlv(&mut out, 0x12, &cnvi_bt.to_le_bytes());
    tlv(&mut out, 0x50, b"unknown entries are skipped");
    tlv(&mut out, 0x1c, &[img_type]);
    tlv(&mut out, 0x2f, &[sbe]);
    tlv(&mut out, 0x2d, &[FW_BUILD.0, FW_BUILD.1, FW_BUILD.2]);
    out
}

/// A controller in its bootloader, speaking the documented protocol.
pub(crate) struct FakeIntel {
    pub legacy: Vec<u8>,
    pub tlv: Vec<u8>,
    pub boot_params: Vec<u8>,
    pub body_len: usize,
    /// `(fragment index, status)`: that Secure_Send is refused.
    pub refuse_fragment: Option<(usize, u8)>,
    /// The bootloader's verdict byte; `None` never sends one.
    pub verdict: Option<u8>,
    pub bootup: bool,
    /// Status the Intel Reset that boots the image answers with.
    pub refuse_boot_reset: Option<u8>,
    /// The record index whose Write_DDC is refused.
    pub refuse_ddc: Option<usize>,
    pub fragments: Vec<(u8, Vec<u8>)>,
    data_seen: usize,
}

impl FakeIntel {
    pub fn new(legacy: Vec<u8>, tlv: Vec<u8>, body_len: usize) -> FakeIntel {
        FakeIntel {
            legacy,
            tlv,
            boot_params: boot_params(0, 0),
            body_len,
            refuse_fragment: None,
            verdict: Some(0),
            bootup: true,
            refuse_boot_reset: None,
            refuse_ddc: None,
            fragments: Vec::new(),
            data_seen: 0,
        }
    }

    /// The octets of the fragments of one type, concatenated.
    pub fn stream(&self, ty: u8) -> Vec<u8> {
        self.fragments
            .iter()
            .filter(|(t, _)| *t == ty)
            .flat_map(|(_, d)| d.iter().copied())
            .collect()
    }
}

impl Fake for FakeIntel {
    fn respond(&mut self, opcode: u16, params: &[u8]) -> Vec<Vec<u8>> {
        match (opcode, params) {
            (READ_VERSION, [0xff]) => vec![cc(opcode, &self.tlv)],
            (READ_VERSION, []) => vec![cc(opcode, &self.legacy)],
            (BOOT_PARAMS, _) => vec![cc(opcode, &self.boot_params)],
            (SECURE_SEND, [ty, data @ ..]) => {
                let n = self.fragments.len();
                self.fragments.push((*ty, data.to_vec()));
                if let Some((at, status)) = self.refuse_fragment {
                    if at == n {
                        return vec![cc(opcode, &[status])];
                    }
                }
                let mut out = vec![cc(opcode, &[0])];
                if *ty == 1 {
                    self.data_seen += data.len();
                }
                if *ty == 1 && self.data_seen == self.body_len {
                    if let Some(result) = self.verdict {
                        out.push(vendor_event(&[0x06, result, 0x09, 0xfc, 0]));
                    }
                }
                out
            }
            (RESET, [0, 1, 0, 1, ..]) => {
                if let Some(status) = self.refuse_boot_reset {
                    return vec![cc(opcode, &[status])];
                }
                let mut out = vec![cc(opcode, &[0])];
                if self.bootup {
                    out.push(vendor_event(&[0x02, 0, 5, 1, 0, 0, 1]));
                }
                out
            }
            (RESET, _) => vec![cc(opcode, &[0])],
            (WRITE_DDC, _) => {
                self.fragments.push((0xdd, params.to_vec()));
                let written = self.fragments.iter().filter(|f| f.0 == 0xdd).count();
                match self.refuse_ddc {
                    Some(at) if written == at + 1 => vec![cc(opcode, &[0x12])],
                    _ => vec![cc(opcode, &[0])],
                }
            }
            _ => vec![cc(opcode, &[1])],
        }
    }
}

pub(crate) fn files(entries: &[(&str, Vec<u8>)]) -> Files {
    entries
        .iter()
        .map(|(n, d)| (std::string::String::from(*n), d.clone()))
        .collect::<BTreeMap<_, _>>()
}

pub(crate) fn play(fake: &mut FakeIntel, files: &Files) -> (Loader, Run) {
    let mut loader = Loader::new(Vendor::Intel);
    let run = drive(&mut loader, fake, files, 0, Input::Start);
    (loader, run)
}

/// Two DDC records, as the real files carry.
pub(crate) const DDC: [u8; 9] = [0x03, 0x28, 0x01, 0x18, 0x04, 0x29, 0x01, 0x03, 0x00];

#[test]
fn a_legacy_bootloader_part_is_loaded_booted_and_configured() {
    // hw_variant 0x13 (AX201): the file is ibt-<hw>-<hw_rev>-<fw_rev>.
    let body = body((1, 2, 3));
    let mut fake = FakeIntel::new(
        legacy_version(0x13, 0x06, (0, 0, 0)),
        Vec::new(),
        body.len(),
    );
    let files = files(&[
        ("intel/ibt-19-0-4.sfi", sfi(false, &body)),
        ("intel/ibt-19-0-4.ddc", DDC.to_vec()),
    ]);
    // The first answer comes to the TLV-style query: a legacy part sends the
    // ten-octet record to it as well.
    fake.tlv = fake.legacy.clone();
    let (_, run) = play(&mut fake, &files);

    assert_eq!(
        run.files,
        [
            ("intel/ibt-19-0-4.sfi".into(), true),
            ("intel/ibt-19-0-4.ddc".into(), false)
        ]
    );
    assert_eq!(
        run.last,
        Step::Done(Done::Operational {
            downloaded: true,
            config: Config::Applied(2)
        })
    );
    // The signed header goes first, in the documented pieces (252 max).
    let kinds: Vec<u8> = fake.fragments.iter().map(|f| f.0).collect();
    assert_eq!(&kinds[..5], [0, 3, 3, 2, 2]);
    let file = sfi(false, &body);
    assert_eq!(fake.stream(0), file[0..128]);
    assert_eq!(fake.stream(3), file[128..384]);
    assert_eq!(fake.stream(2), file[388..644]);
    assert_eq!(
        fake.stream(1),
        body,
        "the command stream, whole and in order"
    );
    assert!(fake.fragments.iter().all(|(_, d)| d.len() <= 252));
    // Reset with the boot address from the image, then the DDC records whole.
    assert_eq!(run.params(RESET)[0], [0, 1, 0, 1, 0x00, 0x08, 0x10, 0x00]);
    assert_eq!(fake.stream(0xdd), DDC);
    assert_eq!(run.count(BOOT_PARAMS), 1);
}

#[test]
fn legacy_names_the_oldest_bootloaders_by_device_revision() {
    let body = body((1, 2, 3));
    let mut fake = FakeIntel::new(
        legacy_version(0x0c, 0x06, (0, 0, 0)),
        Vec::new(),
        body.len(),
    );
    fake.tlv = fake.legacy.clone();
    fake.boot_params = boot_params(16, 0);
    let files = files(&[("intel/ibt-12-16.sfi", sfi(false, &body))]);
    let (_, run) = play(&mut fake, &files);
    assert_eq!(run.files[0].0, "intel/ibt-12-16.sfi");
    // No DDC file in the tree: the controller works without it.
    assert_eq!(
        run.last,
        Step::Done(Done::Operational {
            downloaded: true,
            config: Config::Missing
        })
    );
}

fn tlv_part(sbe: u8) -> (FakeIntel, Files, Vec<u8>) {
    // AX210: CNVi and CNVr type 0x410 at step 0 -> ibt-0041-0041.
    let body = body((9, 9, 9));
    let tlv = tlv_version(0x19, 1, sbe, (0x410, 0x410));
    let fake = FakeIntel::new(Vec::new(), tlv, body.len());
    let files = files(&[
        ("intel/ibt-0041-0041.sfi", sfi(true, &body)),
        ("intel/ibt-0041-0041.ddc", DDC.to_vec()),
    ]);
    (fake, files, body)
}

#[test]
fn a_tlv_part_sends_the_rsa_header_when_the_engine_is_rsa() {
    let (mut fake, files, body) = tlv_part(0);
    let (_, run) = play(&mut fake, &files);
    assert_eq!(run.files[0].0, "intel/ibt-0041-0041.sfi");
    assert!(matches!(
        run.last,
        Step::Done(Done::Operational {
            downloaded: true,
            ..
        })
    ));
    let file = sfi(true, &body);
    assert_eq!(fake.stream(0), file[0..128]);
    assert_eq!(fake.stream(3), file[128..384]);
    assert_eq!(fake.stream(2), file[388..644]);
    assert_eq!(fake.stream(1), body, "the commands start after both blocks");
    assert_eq!(
        run.count(BOOT_PARAMS),
        0,
        "TLV parts have no boot parameters"
    );
}

#[test]
fn a_tlv_part_sends_the_ecdsa_header_when_the_engine_is_ecdsa() {
    let (mut fake, files, body) = tlv_part(1);
    let (_, run) = play(&mut fake, &files);
    assert!(matches!(
        run.last,
        Step::Done(Done::Operational {
            downloaded: true,
            ..
        })
    ));
    let file = sfi(true, &body);
    assert_eq!(fake.stream(0), file[644..772]);
    assert_eq!(fake.stream(3), file[772..868]);
    assert_eq!(fake.stream(2), file[868..964]);
    assert_eq!(fake.stream(1), body);
}

#[test]
fn the_cnvx_words_name_the_file_with_type_and_step_swapped() {
    // AX211: CNVi type 0x400, CNVr type 0x410 at step 1 -> ibt-0040-0141.
    let tlv = tlv_version(0x1b, 1, 0, (0x400, 0x410 | 1 << 24));
    let mut fake = FakeIntel::new(Vec::new(), tlv, 0);
    let (_, run) = play(&mut fake, &Files::new());
    assert_eq!(run.files[0].0, "intel/ibt-0040-0141.sfi");
    assert!(run.files[0].1, "the image is required");
}

#[test]
fn operational_firmware_needs_no_file_and_no_download() {
    let mut tlv_fake = FakeIntel::new(Vec::new(), tlv_version(0x19, 3, 0, (0x410, 0x410)), 0);
    let (_, run) = play(&mut tlv_fake, &Files::new());
    assert!(run.files.is_empty());
    assert_eq!(run.opcodes(), [READ_VERSION]);
    assert_eq!(
        run.last,
        Step::Done(Done::Operational {
            downloaded: false,
            config: Config::NotUsed
        })
    );

    let mut legacy_fake = FakeIntel::new(legacy_version(0x13, 0x23, (1, 1, 1)), Vec::new(), 0);
    legacy_fake.tlv = legacy_fake.legacy.clone();
    let (_, run) = play(&mut legacy_fake, &Files::new());
    assert!(matches!(
        run.last,
        Step::Done(Done::Operational {
            downloaded: false,
            ..
        })
    ));
}

#[test]
fn a_middle_generation_part_answering_tlv_is_asked_again_the_legacy_way() {
    // Hardware variant 0x13 answers TLV, but its bootloader is named by the
    // legacy record: the loader asks Read_Version again without the 0xFF.
    let body = body((1, 2, 3));
    let mut fake = FakeIntel::new(
        legacy_version(0x13, 0x06, (0, 0, 0)),
        tlv_version(0x13, 1, 0, (0x410, 0x410)),
        body.len(),
    );
    let files = files(&[("intel/ibt-19-0-4.sfi", sfi(false, &body))]);
    let (_, run) = play(&mut fake, &files);
    assert_eq!(
        run.opcodes()[..3],
        [READ_VERSION, READ_VERSION, BOOT_PARAMS]
    );
    assert_eq!(run.params(READ_VERSION), [&[0xffu8][..], &[][..]]);
    assert!(matches!(
        run.last,
        Step::Done(Done::Operational {
            downloaded: true,
            ..
        })
    ));
}

#[test]
fn an_image_the_controller_already_holds_is_booted_without_a_download() {
    let body = body(FW_BUILD);
    let tlv = tlv_version(0x19, 1, 0, (0x410, 0x410));
    let mut fake = FakeIntel::new(Vec::new(), tlv, body.len());
    let files = files(&[("intel/ibt-0041-0041.sfi", sfi(true, &body))]);
    let (_, run) = play(&mut fake, &files);
    assert_eq!(run.count(SECURE_SEND), 0);
    assert_eq!(run.params(RESET)[0][4..], BOOT_ADDR.to_le_bytes());
    assert!(matches!(
        run.last,
        Step::Done(Done::Operational {
            downloaded: false,
            ..
        })
    ));
}

#[test]
fn the_reset_that_boots_the_image_may_take_the_usb_device_with_it() {
    let (mut fake, files, _) = tlv_part(0);
    fake.bootup = false;
    let (mut loader, run) = play(&mut fake, &files);
    assert_eq!(run.last, Step::Wait, "waiting for the boot-up event");
    assert!(loader.deadline().is_some());
    let step = loader.step(1, Input::Detached);
    assert_eq!(step, Step::Done(Done::Reenumerate));
    assert_eq!(
        loader.step(2, Input::Tick),
        Step::Wait,
        "finished stays finished"
    );
}
