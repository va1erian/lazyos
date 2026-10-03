//! Route strings, transaction translators and the hub fields of the slot
//! context, in both context sizes (QEMU's `qemu-xhci` only has 32-byte
//! contexts, so the 64-byte layout is proven here).

use std::format;
use std::vec;

use crate::context::{EndpointContext, EndpointType, HubSlot, InputContext, SlotContext, Tt};
use crate::regs::Speed;
use crate::route::{Location, MAX_TIERS};
use crate::Error;

#[test]
fn route_strings_add_one_nibble_per_tier() {
    let root = Location::root(3, Speed::High);
    let hub = root.child(1, false, 2, Speed::High).unwrap();
    let below = hub.child(2, false, 15, Speed::High).unwrap();
    assert_eq!((hub.route, hub.depth), (0x2, 1));
    assert_eq!((below.route, below.depth, below.root_port), (0xF2, 2, 3));
    assert_eq!(format!("{root} {hub} {below}"), "3 3.2 3.2.15");
    assert!(below.is_within(&hub) && below.is_within(&root) && hub.is_within(&hub));
    assert!(!hub.is_within(&below));
    let sibling = root.child(1, false, 4, Speed::High).unwrap();
    assert!(!below.is_within(&sibling));
    assert!(!below.is_within(&Location::root(4, Speed::High)));
    // Five tiers is the limit; port 0 and ports above 15 cannot be named.
    let mut deep = root;
    for tier in 0..MAX_TIERS {
        deep = deep.child(tier + 1, false, 1, Speed::High).unwrap();
    }
    assert_eq!(deep.route, 0x11111);
    assert_eq!(
        deep.child(9, false, 1, Speed::High),
        Err(Error::BadArgument)
    );
    assert_eq!(
        root.child(1, false, 0, Speed::High),
        Err(Error::BadArgument)
    );
    assert_eq!(
        root.child(1, false, 16, Speed::High),
        Err(Error::BadArgument)
    );
    assert_eq!(
        root.child(0, false, 1, Speed::High),
        Err(Error::BadArgument)
    );
}

#[test]
fn slow_devices_behind_a_high_speed_hub_use_its_tt() {
    let hs_hub = Location::root(1, Speed::High)
        .child(1, false, 3, Speed::High)
        .unwrap();
    let keyboard = hs_hub.child(7, true, 2, Speed::Low).unwrap();
    assert_eq!(
        keyboard.tt,
        Some(Tt {
            hub_slot: 7,
            port: 2,
            multi: true
        })
    );
    // A full-speed hub behind it shares that TT with everything below it.
    let fs_hub = hs_hub.child(7, false, 4, Speed::Full).unwrap();
    let mouse = fs_hub.child(8, false, 1, Speed::Full).unwrap();
    assert_eq!(mouse.tt, fs_hub.tt);
    assert_eq!(mouse.tt.unwrap().hub_slot, 7);
    // A full-speed hub on a root port has no TT at all.
    let root_fs = Location::root(2, Speed::Full)
        .child(1, false, 1, Speed::Full)
        .unwrap();
    assert_eq!(root_fs.tt, None);
    // Speeds a hub cannot carry.
    assert!(Location::root(2, Speed::Full)
        .child(1, false, 1, Speed::High)
        .is_err());
    assert!(Location::root(2, Speed::Super)
        .child(1, false, 1, Speed::High)
        .is_err());
    assert!(Location::root(2, Speed::High)
        .child(1, false, 1, Speed::Super)
        .is_err());
    let ss = Location::root(5, Speed::Super)
        .child(1, false, 1, Speed::Super)
        .unwrap();
    assert_eq!((ss.route, ss.tt), (1, None));
}

#[test]
fn hub_and_tt_fields_in_both_context_sizes() {
    for csz64 in [false, true] {
        let stride = if csz64 { 16 } else { 8 };
        let mut buffer = vec![0xA5A5_A5A5u32; 33 * stride];
        let mut input = InputContext::new(&mut buffer, csz64).unwrap();
        let hs_hub = Location::root(4, Speed::High)
            .child(1, false, 3, Speed::High)
            .unwrap();
        let hub = HubSlot {
            ports: 7,
            multi_tt: true,
            think_time: 2,
        };
        input.slot(&hs_hub.slot_context(3, Some(hub))).unwrap();
        input
            .endpoint(
                3,
                &EndpointContext::interrupt(EndpointType::InterruptIn, 1, 0, 12, 0x8000 | 1),
            )
            .unwrap();
        let d = input.dwords();
        let slot = stride;
        assert_eq!(
            d[slot],
            3 | 3 << 20 | 1 << 25 | 1 << 26 | 3 << 27,
            "route, speed, MTT, Hub, entries"
        );
        assert_eq!(d[slot + 1], 4 << 16 | 7 << 24, "root port, ports");
        assert_eq!(d[slot + 2], 2 << 16, "think time");
        // DCI 3 is context 4: 4 * 32 or 4 * 64 bytes in.
        let ep = 4 * stride;
        assert_eq!(d[ep], 12 << 16);
        assert_eq!(d[ep + 4], 1 | 1 << 16, "average TRB, max ESIT");
        assert!(
            d[ep + 5..ep + stride].iter().all(|&w| w == 0),
            "padding cleared"
        );

        // A low-speed keyboard behind that hub's port 2.
        let mut buffer = vec![0u32; 33 * stride];
        let mut input = InputContext::new(&mut buffer, csz64).unwrap();
        let keyboard = hs_hub.child(9, true, 2, Speed::Low).unwrap();
        input.slot(&keyboard.slot_context(1, None)).unwrap();
        let d = input.dwords();
        assert_eq!(d[slot], 0x23 | 2 << 20 | 1 << 25 | 1 << 27);
        assert_eq!(d[slot + 2], 9 | 2 << 8, "TT hub slot and port");
    }
}

#[test]
fn slot_and_endpoint_contexts_refuse_bad_fields() {
    let mut buffer = vec![0u32; 33 * 8];
    let mut input = InputContext::new(&mut buffer, false).unwrap();
    let base = SlotContext::root(Speed::Full, 1);
    let tt = Tt {
        hub_slot: 0,
        port: 1,
        multi: false,
    };
    assert!(input
        .slot(&SlotContext {
            tt: Some(tt),
            ..base
        })
        .is_err());
    let hub = HubSlot {
        ports: 0,
        multi_tt: false,
        think_time: 0,
    };
    assert!(input
        .slot(&SlotContext {
            hub: Some(hub),
            ..base
        })
        .is_err());
    let hub = HubSlot {
        ports: 4,
        think_time: 4,
        ..hub
    };
    assert!(input
        .slot(&SlotContext {
            hub: Some(hub),
            ..base
        })
        .is_err());
    let mut ep = EndpointContext::interrupt(EndpointType::InterruptIn, 1024, 2, 3, 0x1000);
    assert_eq!(
        ep.max_esit, 3072,
        "high-bandwidth: three packets per interval"
    );
    input.endpoint(3, &ep).unwrap();
    let d = input.dwords();
    assert_eq!((d[4 * 8 + 1] >> 8) & 0xFF, 2, "max burst");
    ep.max_esit = 1 << 24;
    assert_eq!(input.endpoint(3, &ep), Err(Error::BadArgument));
    let bulk = EndpointContext::bulk(EndpointType::BulkIn, 512, 0, 0x2000 | 1);
    input.endpoint(3, &bulk).unwrap();
    let d = input.dwords();
    assert_eq!(d[4 * 8 + 1], 3 << 1 | 6 << 3 | 512 << 16);
    assert_eq!(d[4 * 8 + 4], 3072, "bulk: average 3072, no ESIT");
}
