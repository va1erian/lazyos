//! Isochronous transfers (USB audio): the Isoch TRB, the isochronous
//! endpoint context, drop flags for an alternate switched off, the
//! interval encoding and the audio class requests.

use super::RING_PHYS;
use crate::context::{EndpointContext, EndpointType, InputContext};
use crate::regs::{isoch_interval, Speed};
use crate::setup::request::audio;
use crate::trb::{self, kind, IOC, ISP, SIA};
use crate::Error;
use std::vec;

#[test]
fn isoch_trb_layout() {
    let out = trb::isoch(0x2000, 192, 1, true, false).unwrap();
    assert_eq!(
        (out.kind(), out.parameter, out.status),
        (kind::ISOCH, 0x2000, 192)
    );
    assert_eq!(out.control & (SIA | IOC | ISP), SIA | IOC);
    assert_eq!((out.control >> 16) & 0xF, 0, "TLBPC: one packet");
    assert_eq!((out.control >> 7) & 0b11, 0, "TBC: one burst");
    let three = trb::isoch(0x3000, 3000, 3, false, true).unwrap();
    assert_eq!((three.control >> 16) & 0xF, 2);
    assert_eq!(three.control & (IOC | ISP), ISP);
    assert_eq!(trb::isoch(0, 1 << 17, 1, false, false), None);
    assert_eq!(trb::isoch(0, 4, 0, false, false), None);
}

#[test]
fn isoch_contexts_have_no_error_count() {
    let mut buffer = vec![0u32; 33 * 8];
    let mut input = InputContext::new(&mut buffer, false).unwrap();
    let out = EndpointContext::isoch(EndpointType::IsochOut, 196, 0, 3, 196, RING_PHYS | 1);
    let feedback = EndpointContext::isoch(EndpointType::IsochIn, 4, 0, 6, 4, RING_PHYS | 1);
    input.endpoint(2, &out).unwrap();
    input.endpoint(3, &feedback).unwrap();
    input.drop_endpoint(4).unwrap();
    assert_eq!(input.drop_endpoint(1), Err(Error::BadArgument));
    let dwords = input.dwords();
    let ep = |dci: usize| &dwords[(dci + 1) * 8..(dci + 2) * 8];
    assert_eq!((ep(2)[1] >> 3) & 7, 1, "isoch OUT is type 1");
    assert_eq!((ep(3)[1] >> 3) & 7, 5, "isoch IN is type 5");
    assert_eq!((ep(2)[1] >> 1) & 3, 0, "CErr must be 0");
    assert_eq!((ep(2)[0] >> 16) & 0xFF, 3, "interval");
    assert_eq!(ep(2)[4], 196 | 196 << 16, "average TRB and ESIT payload");
    assert_eq!(dwords[0], 1 << 4, "drop flags");
    assert_eq!(input.added(), 1 << 2 | 1 << 3);
}

#[test]
fn isoch_intervals() {
    assert_eq!(isoch_interval(Speed::Full, 1), 3, "1 ms");
    assert_eq!(isoch_interval(Speed::Full, 4), 6, "8 ms");
    assert_eq!(isoch_interval(Speed::High, 1), 0, "125 us");
    assert_eq!(isoch_interval(Speed::High, 4), 3);
    assert_eq!(isoch_interval(Speed::High, 0), 0);
    assert_eq!(isoch_interval(Speed::Full, 16), 15);
}

#[test]
fn audio_requests() {
    let rate = audio::uac1_set_rate(0x01);
    assert_eq!(rate.immediate_bytes(), [0x22, 0x01, 0, 0x01, 0x01, 0, 3, 0]);
    let clock = audio::uac2_set_rate(0, 0x29);
    assert_eq!(
        clock.immediate_bytes(),
        [0x21, 0x01, 0, 0x01, 0, 0x29, 4, 0]
    );
    let range = audio::uac2_rate_ranges(0, 0x29, 2);
    assert!(range.is_in());
    assert_eq!(
        range.immediate_bytes(),
        [0xA1, 0x02, 0, 0x01, 0, 0x29, 2, 0]
    );
    let mute = audio::set_mute(0, 2);
    assert_eq!(mute.immediate_bytes(), [0x21, 0x01, 0, 0x01, 0, 0x02, 1, 0]);
    assert!(audio::uac1_get_rate(0x01).is_in());
}
