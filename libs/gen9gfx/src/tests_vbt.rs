//! VBT parsing: header, block walk, child devices and their version gates.

use crate::build::{self, ChildSpec};
use crate::vbt::{block, device_type, dvo_port, OutputKind, Port, Vbt, VbtError};

fn lspcon_dp() -> ChildSpec {
    ChildSpec {
        handle: 0x0008,
        device_type: device_type::DP,
        dvo_port: dvo_port::DPB,
        i2c_pin: 0,
        ddc_pin: 5,
        // lspcon (bit 2) + efp_routed (bit 0); DP and HDMI support flags.
        flags23: 0x05,
        flags24: 0x03,
        aux: 0x10,
    }
}

#[test]
fn typical_board() {
    let bytes = build::vbt(210, &build::typical_children());
    let v = Vbt::parse(&bytes).unwrap();
    assert_eq!(v.version, 155);
    assert_eq!(v.bdb_version, 210);
    assert_eq!(v.bdb_offset(), 48);
    assert!(v.checksum_ok);

    let ids: std::vec::Vec<u8> = v.blocks().map(|(id, _)| id).collect();
    assert_eq!(
        ids,
        [block::GENERAL_FEATURES, block::SKIP, block::GENERAL_DEFINITIONS]
    );
    assert!(v.block(block::EDP).is_none());

    let kids = v.children();
    assert_eq!(kids.len(), 2);
    let hdmi = kids[0];
    assert_eq!(hdmi.kind, OutputKind::Hdmi);
    assert_eq!(hdmi.port, Some(Port::B));
    assert_eq!(hdmi.ddc_pin, 5);
    assert_eq!(crate::vbt::gmbus_pin(hdmi.ddc_pin), Some(5));
    assert!(hdmi.hdmi_support && hdmi.tmds_support && !hdmi.dp_support);
    assert!(hdmi.can_hdmi() && !hdmi.lspcon);
    assert_eq!(hdmi.aux_channel, 0);

    let dp = kids[1];
    assert_eq!(dp.kind, OutputKind::DisplayPort);
    assert_eq!(dp.port, Some(Port::C));
    assert_eq!(dp.aux_channel, 0x20);
    assert!(dp.dp_support && !dp.hdmi_support);
    assert!(!dp.can_hdmi());

    assert_eq!(v.hdmi_child().unwrap().port, Some(Port::B));
}

#[test]
fn lspcon_is_visible_only_from_bdb_192() {
    let new = build::vbt(210, &[lspcon_dp()]);
    let c = Vbt::parse(&new).unwrap().children()[0];
    assert_eq!(c.kind, OutputKind::DisplayPort);
    assert!(c.lspcon && c.can_hdmi());
    assert!(!c.lane_reversal);

    // Same bytes, older BDB: the bit means something else (or nothing).
    let old = build::vbt(180, &[lspcon_dp()]);
    let c = Vbt::parse(&old).unwrap().children()[0];
    assert!(!c.lspcon && !c.can_hdmi());

    // Below 158 the flag bytes are not read; support is derived from the
    // device type and AUX is unknown.
    let older = build::vbt(150, &[lspcon_dp()]);
    let c = Vbt::parse(&older).unwrap().children()[0];
    assert_eq!(c.aux_channel, 0);
    assert!(c.dp_support && !c.hdmi_support && !c.tmds_support);
}

#[test]
fn classifies_device_types() {
    let mk = |device_type, dvo_port| ChildSpec {
        handle: 1,
        device_type,
        dvo_port,
        i2c_pin: 0,
        ddc_pin: 0,
        flags23: 0,
        flags24: 0,
        aux: 0,
    };
    let bytes = build::vbt(
        210,
        &[
            mk(device_type::HDMI, dvo_port::HDMIA),
            // TMDS but "not an HDMI output": DVI.
            mk(device_type::HDMI | device_type::NOT_HDMI_OUTPUT, dvo_port::HDMIC),
            mk(device_type::DP_DUAL_MODE, dvo_port::DPD),
            mk(device_type::EDP, dvo_port::DPA),
            mk(0, dvo_port::DPE),
            mk(device_type::ANALOG_OUTPUT, dvo_port::CRT),
            mk(0x0008, dvo_port::LVDS),
            mk(device_type::HDMI, dvo_port::HDMIE),
        ],
    );
    let kids = Vbt::parse(&bytes).unwrap().children();
    // The all-zero device type is an unused slot and is skipped.
    let kinds: std::vec::Vec<_> = kids.iter().map(|c| c.kind).collect();
    assert_eq!(
        kinds,
        [
            OutputKind::Hdmi,
            OutputKind::Dvi,
            OutputKind::DpDualMode,
            OutputKind::Edp,
            OutputKind::Crt,
            OutputKind::Lvds,
            OutputKind::Hdmi,
        ]
    );
    let ports: std::vec::Vec<_> = kids.iter().map(|c| c.port).collect();
    assert_eq!(
        ports,
        [
            Some(Port::A),
            Some(Port::C),
            Some(Port::D),
            Some(Port::A),
            None,
            None,
            Some(Port::E),
        ]
    );
    assert!(kids[2].can_hdmi());
    assert!(!kids[1].can_hdmi());
}

#[test]
fn refuses_bad_headers() {
    let good = build::vbt(210, &build::typical_children());
    assert_eq!(Vbt::parse(&[]).unwrap_err(), VbtError::TooShort);
    assert_eq!(Vbt::parse(&good[..47]).unwrap_err(), VbtError::TooShort);

    let mut b = good.clone();
    b[0] = b'X';
    assert_eq!(Vbt::parse(&b).unwrap_err(), VbtError::BadSignature);

    // vbt_size beyond the data, and below the headers.
    let mut b = good.clone();
    b[24..26].copy_from_slice(&u16::MAX.to_le_bytes());
    assert_eq!(Vbt::parse(&b).unwrap_err(), VbtError::BadSize);
    let mut b = good.clone();
    b[24..26].copy_from_slice(&40u16.to_le_bytes());
    assert_eq!(Vbt::parse(&b).unwrap_err(), VbtError::BadSize);

    // BDB offset out of the table.
    let mut b = good.clone();
    b[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(Vbt::parse(&b).unwrap_err(), VbtError::BadBdbOffset);

    // BDB signature mangled.
    let mut b = good.clone();
    b[48] = b'b';
    assert_eq!(Vbt::parse(&b).unwrap_err(), VbtError::BadBdbSignature);
}

#[test]
fn damaged_tables_keep_what_precedes_the_damage() {
    let good = build::vbt(210, &build::typical_children());
    let v = Vbt::parse(&good).unwrap();

    // A flipped byte breaks the informational checksum, nothing else.
    let mut b = good.clone();
    let last = b.len() - 1;
    b[last] ^= 1;
    assert!(!Vbt::parse(&b).unwrap().checksum_ok);

    // A block size that runs past the BDB ends the walk before it.
    let mut b = good.clone();
    let bdb = v.bdb_offset() + 22;
    b[bdb + 1] = 0xFF;
    b[bdb + 2] = 0xFF;
    let v2 = Vbt::parse(&b).unwrap();
    assert_eq!(v2.blocks().count(), 0);
    assert!(v2.children().is_empty());

    // A table cut inside the general definitions loses its children only.
    let cut = &good[..good.len() - 20];
    let vc = Vbt::parse(cut);
    assert_eq!(vc.unwrap_err(), VbtError::BadSize);

    // A child entry size too small to hold a DDC pin yields no children.
    let mut b = good.clone();
    let defs_at = b.len() - (5 + 2 * build::CHILD_SIZE);
    b[defs_at + 4] = 10;
    assert!(Vbt::parse(&b).unwrap().children().is_empty());
}

#[test]
fn seeded_fuzz() {
    let seeds = [
        build::vbt(210, &build::typical_children()),
        build::vbt(180, &[lspcon_dp()]),
        build::vbt(120, &[]),
    ];
    fuzzkit::for_seeds("gen9gfx_vbt_mutated", |_, rng| {
        let mut data = rng.pick(&seeds).clone();
        let flips = rng.range(1, 10) as usize;
        rng.flip_bits(&mut data, flips);
        if rng.one_in(4) {
            data.truncate(rng.below(data.len() as u64 + 1) as usize);
        }
        crate::fuzz::vbt_bytes(&data);
        let mut framed = std::vec![2u8];
        framed.extend_from_slice(&data);
        crate::fuzz::run(&framed);
    });
    fuzzkit::for_seeds("gen9gfx_vbt_random", |_, rng| {
        let n = rng.range(0, 400) as usize;
        let mut data = rng.bytes(n);
        // Give the header a valid look half the time so the walk is reached.
        if data.len() >= 80 && rng.one_in(2) {
            data[..4].copy_from_slice(b"$VBT");
            let size = data.len() as u16;
            data[24..26].copy_from_slice(&size.to_le_bytes());
            data[28..32].copy_from_slice(&48u32.to_le_bytes());
            data[48..64].copy_from_slice(b"BIOS_DATA_BLOCK ");
        }
        crate::fuzz::vbt_bytes(&data);
    });
}
