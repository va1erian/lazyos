//! Byte-script fuzzing of the driver engine against a reference model.
//!
//! [`run`] reads its input as a script of operations on a fake device, the
//! engine and an attached client: the device delivers frames, the client pushes
//! and pops, control calls arrive (attach, detach, rx mode, kick), the link
//! flaps, and - as a hostile peer would - the device writes impossible
//! completions and the client scribbles on its rings. It is the libFuzzer
//! target and the body of the seeded tests.
//!
//! **Invariants.** While nobody misbehaves ("clean") every observable result
//! must match the model: which frames reach the client in which order, which
//! reach the wire, every counter, the wake-up events, slot conservation. Once
//! the client scribbles or the device lies, only safety is asserted: no panic,
//! guard pages intact, counters never go backwards, nothing larger than a
//! slot is delivered; a fatal device error ends the script, as it ends the
//! driver.

mod model;
mod script;
#[cfg(test)]
mod seeded;

use std::vec::Vec;

use framering::SLOT_BYTES;

use crate::testdev::bed::{Bed, MAC, OWNER};
use crate::testdev::Which;
use crate::Stats;
use model::{Model, Run};
use script::Script;

/// Interpret `data` as a script; see the module docs for the invariants.
pub fn run(data: &[u8]) {
    let mut script = Script { data, at: 0 };
    let first = script.u8();
    let sizes = [2u16, 4, 16, 64, 256];
    let rx_entries = sizes[usize::from(first % 5)];
    let tx_entries = sizes[usize::from(first / 5 % 5)];
    let mut run = Run {
        bed: Bed::new(rx_entries, tx_entries),
        model: Model {
            mode: 1,
            link: true,
            ..Model::default()
        },
        rx_entries,
        tx_entries,
        tainted: false,
        frame_id: 0,
        last: Stats::default(),
    };
    let mut steps = 0u32;
    while !script.done() && steps < 50_000 {
        steps += 1;
        let op = script.u8();
        match op {
            0..=59 => {
                let len = usize::from(script.u16()) % 1700;
                let dst = match script.u8() % 4 {
                    0 => MAC,
                    1 => [0xFF; 6],
                    2 => [0x01, 0, 0x5E, 0, 0, 1],
                    _ => [0x52, 0x54, 0, 9, 9, 9],
                };
                run.deliver(len, dst);
            }
            60..=69 => {
                // A completion that may lie about its length or carry an
                // offload header: safety only.
                let n = usize::from(script.u8());
                let body: Vec<u8> = (0..n).map(|_| script.u8()).collect();
                let claimed = match script.u8() % 4 {
                    0 => body.len() as u32,
                    1 => SLOT_BYTES as u32 + 1,
                    2 => u32::MAX,
                    _ => u32::from(script.u16()),
                };
                if run.bed.dev.deliver_raw(&body, claimed) {
                    run.tainted = true;
                    run.model.dev_rx.clear();
                }
            }
            70..=99 => {
                if run.pump().is_err() {
                    assert!(run.tainted, "a clean device never makes the driver give up");
                    break;
                }
            }
            100..=139 => {
                let len = Run::tx_frame_len(&mut script);
                run.client_push(len);
            }
            140..=169 => run.client_pop(),
            170..=179 => run.device_transmit(),
            180..=184 => {
                if run.bed.client.is_some() {
                    run.bed.client().rx.arm();
                    if !run.tainted && run.model.attached.is_some() {
                        run.model.rx_armed = true;
                    }
                }
            }
            185..=189 => {
                if run.bed.client.is_some() {
                    let got = run.bed.client().tx.take_notify();
                    if !run.tainted && run.model.attached.is_some() {
                        assert_eq!(
                            got, run.model.tx_armed,
                            "kick exactly when the driver armed"
                        );
                        run.model.tx_armed = false;
                    }
                }
            }
            190..=199 => {
                let owner = OWNER + u64::from(script.u8() % 3);
                let slots = [16u32, 32, 64, 8, 24, 2048][usize::from(script.u8() % 6)];
                run.attach(owner, slots);
            }
            200..=214 => run.control(op, &mut script),
            215..=219 => {
                let up = script.u8().is_multiple_of(2);
                let changed = run.bed.engine.set_link(up);
                if !run.tainted {
                    assert_eq!(changed, up != run.model.link);
                    if changed {
                        run.model.link = up;
                        run.model.link_event = true;
                    }
                }
            }
            220..=229 => run.scribble(&mut script),
            230..=239 => {
                // The device lies about a completion: safety only, and the
                // driver may (correctly) give up.
                let which = if script.u8().is_multiple_of(2) {
                    Which::Rx
                } else {
                    Which::Tx
                };
                match script.u8() % 3 {
                    0 => run
                        .bed
                        .dev
                        .write_used_raw(which, script.u32(), script.u32()),
                    1 => run.bed.dev.set_used_index(which, script.u16()),
                    _ => {
                        run.bed
                            .dev
                            .write_used_raw(which, u32::from(script.u8() % 4), script.u32())
                    }
                }
                run.tainted = true;
            }
            _ => {
                run.bed.engine.release();
                run.model.attached = None;
            }
        }
        if steps.is_multiple_of(32) {
            run.bed.assert_guards();
        }
    }
    run.bed.assert_guards();
}
