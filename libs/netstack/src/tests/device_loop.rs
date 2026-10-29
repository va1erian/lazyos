//! The loopback device (`RingDevice::loopback`): what it sends it receives,
//! in order, and a full queue pushes back on the sender instead of dropping.

use smoltcp::phy::{Device, Medium, RxToken, TxToken};
use smoltcp::time::Instant;

use crate::device::{RingDevice, LOOP_FRAMES};

fn send(device: &mut RingDevice, byte: u8) -> bool {
    match device.transmit(Instant::ZERO) {
        Some(token) => {
            token.consume(4, |buf| buf.fill(byte));
            true
        }
        None => false,
    }
}

#[test]
fn a_loopback_device_receives_what_it_sends_in_order() {
    let mut device = RingDevice::loopback(1514);
    assert_eq!(device.capabilities().medium, Medium::Ip);
    assert!(device.is_attached() && device.is_loopback());
    for byte in 1..=3 {
        assert!(send(&mut device, byte));
    }
    assert_eq!(device.looped_frames(), 3);
    for byte in 1..=3 {
        let (rx, _) = device.receive(Instant::ZERO).expect("a frame");
        rx.consume(|frame| assert_eq!(frame, [byte; 4]));
    }
    assert!(device.receive(Instant::ZERO).is_none());
    assert_eq!(device.stats().tx_frames, 3);
    assert_eq!(device.stats().rx_frames, 3);
}

#[test]
fn a_full_loopback_queue_holds_the_sender_back_and_drops_nothing() {
    let mut device = RingDevice::loopback(1514);
    let mut sent = 0;
    while send(&mut device, sent as u8) {
        sent += 1;
        assert!(sent <= LOOP_FRAMES, "the queue grew past its limit");
    }
    assert_eq!(sent, LOOP_FRAMES);
    assert_eq!(device.stats().tx_dropped, 0);
    // Taking one frame in makes room for one more, and its reply fits.
    let (rx, tx) = device.receive(Instant::ZERO).expect("a frame");
    rx.consume(|frame| assert_eq!(frame, [0; 4]));
    tx.consume(4, |buf| buf.fill(0xEE));
    assert_eq!(device.looped_frames(), LOOP_FRAMES);
    assert!(!send(&mut device, 0));
    assert_eq!(device.stats().tx_dropped, 0);
}
