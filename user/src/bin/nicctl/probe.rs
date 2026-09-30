//! `nicctl probe=1`: hostile and out-of-contract requests to the NIC driver,
//! and `nicctl role=intruder`, the second task that must be refused on the
//! owner's ring.
//!
//! Every check states what the driver must answer; a wrong answer (or a
//! driver that stops answering) fails the probe with the check's name. The
//! frames that must be *dropped* are also sent on the real wire path, so the
//! packet capture proves they never left the machine (`tools/net/run.py`).

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use framering::{ring_bytes, Ring};
use libmessenger::{BufferDesc, Header, Parcel, VERSION};
use user::messenger::net::{self as api, wire, Client};
use user::messenger::{create_pair, errno};
use user::sys;

use super::common::{connect, fail, is_errno, nap};

/// Ticks to wait for the driver to act on something we queued or corrupted.
const SETTLE_TICKS: u64 = 300;

/// The EtherType carried by the probe frames (IEEE 802 local experimental),
/// so the capture analyser can tell them from ARP and from real traffic.
pub(super) const PROBE_ETHERTYPE: [u8; 2] = [0x88, 0xB5];

struct Checks {
    done: u32,
}

impl Checks {
    fn expect(&mut self, name: &str, ok: bool) -> Result<(), String> {
        if ok {
            self.done += 1;
            Ok(())
        } else {
            Err(format!("check failed: {name}"))
        }
    }

    fn refused<T>(
        &mut self,
        name: &str,
        result: Result<T, user::messenger::Error>,
        code: i64,
    ) -> Result<(), String> {
        self.expect(&format!("{name} -> errno {code}"), is_errno(&result, code))
    }
}

/// A well-formed `AttachRing` request whose pieces the probe can damage:
/// `slots` in the body, a shared buffer of `buffer_bytes` whose rings are
/// initialised for `init_slots` (0 leaves the header zeroed), a declared range
/// of `declared` bytes, and optionally no buffer or no endpoint.
struct Attempt {
    slots: u32,
    buffer_bytes: usize,
    init_slots: u32,
    declared: usize,
    with_buffer: bool,
    with_endpoint: bool,
}

impl Attempt {
    fn good(slots: u32) -> Attempt {
        let bytes = ring_bytes(slots) * 2;
        Attempt {
            slots,
            buffer_bytes: bytes,
            init_slots: slots,
            declared: bytes,
            with_buffer: true,
            with_endpoint: true,
        }
    }

    /// Send it; whatever the driver refused is cleaned up here.
    fn send(&self, client: &Client) -> Result<Parcel, user::messenger::Error> {
        let mut handles = Vec::new();
        let mut buffers = Vec::new();
        let mut buffer = None;
        if self.with_buffer {
            let size = self.buffer_bytes.max(4096) as u64;
            let (handle, va) = sys::display_create_buffer(size)
                .map_err(|code| user::messenger::Error::Errno(-code))?;
            buffer = Some(handle);
            let one = ring_bytes(self.init_slots);
            if one != 0 && self.buffer_bytes >= one * 2 {
                let base = va as *mut u8;
                // SAFETY: the buffer is at least two rings long and mapped.
                unsafe {
                    let _ = Ring::create(base, one, self.init_slots);
                    let _ = Ring::create(base.add(one), one, self.init_slots);
                }
            }
            buffers.push(BufferDesc {
                handle,
                offset: 0,
                len: self.declared as u64,
                flags: 0,
            });
        }
        let mut pair = None;
        if self.with_endpoint {
            let (mine, theirs) = create_pair()?;
            handles.push(theirs.handle());
            pair = Some((mine, theirs));
        }
        let body = wire::encode_attach_ring_args(&wire::AttachRingArgs { slots: self.slots })
            .map_err(user::messenger::Error::Parcel)?;
        let result = client.raw(wire::METHOD_ATTACHRING, body, handles, buffers);
        if result.is_err() {
            // The driver closed its copies; ours are closed here.
            if let Some(handle) = buffer {
                let _ = sys::display_close_buffer(handle);
            }
            if let Some((mine, theirs)) = pair {
                let _ = mine.close();
                let _ = theirs.close();
            }
        }
        result
    }
}

/// An Ethernet frame of `len` bytes from `mac` to broadcast, EtherType
/// [`PROBE_ETHERTYPE`], payload a recognisable pattern. A frame shorter than
/// the header is cut from that template, so a 13-byte frame is exactly a
/// header missing its last byte.
fn probe_frame(mac: &[u8; 6], len: usize) -> Vec<u8> {
    let mut frame = vec![0u8; len];
    for (i, byte) in frame.iter_mut().enumerate() {
        *byte = match i {
            0..=5 => 0xFF,
            6..=11 => mac[i - 6],
            12 => PROBE_ETHERTYPE[0],
            13 => PROBE_ETHERTYPE[1],
            _ => (i as u8) ^ 0x5A,
        };
    }
    frame
}

/// Run the probe; returns how many checks passed.
pub(super) fn run() -> Result<u32, String> {
    let client = connect()?;
    let mut checks = Checks { done: 0 };
    let info = client.info().map_err(fail("info"))?;
    let mac: [u8; 6] = info
        .mac
        .as_slice()
        .try_into()
        .map_err(|_| String::from("bad MAC"))?;

    // --- Nothing attached: refusals that need no ring. -------------------
    let foreign = Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: 0x1234_5678_9ABC_DEF0,
            method: wire::METHOD_INFO,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        ..Parcel::default()
    };
    checks.refused(
        "a call on a foreign interface id",
        client.call_parcel(foreign),
        errno::EINVAL,
    )?;
    checks.refused(
        "an unknown method",
        client.raw(0xDEAD_BEEF, Vec::new(), Vec::new(), Vec::new()),
        errno::EINVAL,
    )?;
    checks.refused(
        "SetRxMode with nothing attached",
        client.set_rx_mode(1),
        errno::EINVAL,
    )?;
    checks.refused(
        "DetachRing with nothing attached",
        client.detach(1),
        errno::EINVAL,
    )?;
    // A body that is not a valid request.
    checks.refused(
        "AttachRing with an empty body",
        client.raw(wire::METHOD_ATTACHRING, Vec::new(), Vec::new(), Vec::new()),
        errno::EINVAL,
    )?;
    let mut no_buffer = Attempt::good(16);
    no_buffer.with_buffer = false;
    checks.refused(
        "AttachRing without a buffer",
        no_buffer.send(&client),
        errno::EINVAL,
    )?;
    let mut no_endpoint = Attempt::good(16);
    no_endpoint.with_endpoint = false;
    checks.refused(
        "AttachRing without a notify endpoint",
        no_endpoint.send(&client),
        errno::EINVAL,
    )?;
    for slots in [0u32, 1, 8, 24, 2048, u32::MAX] {
        let mut bad = Attempt::good(16);
        bad.slots = slots;
        checks.refused(
            &format!("AttachRing with {slots} slots"),
            bad.send(&client),
            errno::EINVAL,
        )?;
    }
    let exact = ring_bytes(16) * 2;
    for (name, declared) in [
        ("one byte short", exact - 1),
        ("one byte long", exact + 1),
        ("a single ring", exact / 2),
        ("empty", 0),
    ] {
        let mut bad = Attempt::good(16);
        bad.declared = declared;
        checks.refused(
            &format!("AttachRing with a range {name}"),
            bad.send(&client),
            errno::EINVAL,
        )?;
    }
    let mut blank = Attempt::good(16);
    blank.init_slots = 0;
    checks.refused(
        "AttachRing over rings nobody initialised",
        blank.send(&client),
        errno::EINVAL,
    )?;
    let mut wrong_geometry = Attempt::good(16);
    wrong_geometry.init_slots = 32;
    wrong_geometry.buffer_bytes = ring_bytes(32) * 2;
    checks.refused(
        "AttachRing over rings of another size",
        wrong_geometry.send(&client),
        errno::EINVAL,
    )?;

    // --- A valid attachment, then abuse of it. ---------------------------
    let mut attachment = client.attach(16).map_err(fail("a valid attach"))?;
    checks.expect("a valid attach", attachment.ring != 0)?;
    checks.refused(
        "a second AttachRing",
        Attempt::good(16).send(&client),
        errno::EBUSY,
    )?;
    checks.refused(
        "SetRxMode with an unknown mode",
        client.set_rx_mode(9),
        errno::EINVAL,
    )?;
    checks.expect(
        "SetRxMode Promiscuous",
        client
            .set_rx_mode(wire::RX_MODE_PROMISCUOUS)
            .map_err(fail("rx mode"))?,
    )?;
    checks.expect(
        "SetRxMode Filtered",
        client
            .set_rx_mode(wire::RX_MODE_FILTERED)
            .map_err(fail("rx mode"))?,
    )?;
    checks.refused(
        "DetachRing with the wrong ring id",
        client.detach(attachment.ring + 1),
        errno::EINVAL,
    )?;

    // The frame-length policy, on the real transmit path: only the two frames
    // at the legal extremes (14 and 1514 bytes) may reach the wire.
    let before = client.stats().map_err(fail("stats"))?;
    for len in [13usize, 14, 1514, 1515] {
        let pushed = attachment.tx.push(&probe_frame(&mac, len));
        checks.expect(
            &format!("the client's own ring accepts a {len}-byte frame"),
            pushed.is_ok(),
        )?;
    }
    client.kick(attachment.ring).map_err(fail("kick"))?;
    let settled = wait_for(&client, |s| {
        s.tx_frames >= before.tx_frames + 2
            && s.runts > before.runts
            && s.oversize > before.oversize
    })?;
    checks.expect(
        "exactly two of the four frames were transmitted",
        settled.tx_frames == before.tx_frames + 2,
    )?;
    checks.expect(
        "the 13-byte frame was counted as a runt",
        settled.runts == before.runts + 1,
    )?;
    checks.expect(
        "the 1515-byte frame was counted as oversize",
        settled.oversize == before.oversize + 1,
    )?;
    checks.expect(
        "the dropped frames were counted as dropped",
        settled.tx_dropped == before.tx_dropped + 2,
    )?;
    checks.expect(
        "no ring error yet",
        settled.ring_errors == before.ring_errors,
    )?;

    // A second task must be refused on the owner's ring.
    checks.expect(
        "the intruder task ran clean",
        run_intruder(attachment.ring)?,
    )?;

    // The owner corrupts its own ring: the driver must drop it, not crash.
    attachment.corrupt_transmit_ring();
    client.kick(attachment.ring).map_err(fail("kick"))?;
    let deadline = sys::clock() + SETTLE_TICKS;
    while attachment.is_live() && sys::clock() < deadline {
        nap();
    }
    checks.expect(
        "a corrupt ring makes the driver drop the client",
        !attachment.is_live(),
    )?;
    let after = client.stats().map_err(fail("stats after corruption"))?;
    checks.expect(
        "the corrupt ring was counted",
        after.ring_errors == settled.ring_errors + 1,
    )?;
    checks.refused(
        "DetachRing after the driver dropped the client",
        client.detach(attachment.ring),
        errno::EINVAL,
    )?;
    attachment.close();

    // The driver is still there, and a new client can attach.
    let again = client
        .attach(16)
        .map_err(fail("re-attach after a corrupt ring"))?;
    checks.expect("a new attach after a corrupt ring", again.ring != 0)?;
    client.detach(again.ring).map_err(fail("detach"))?;
    again.close();
    client.info().map_err(fail("info at the end"))?;
    checks.expect("the driver still answers", true)?;
    Ok(checks.done)
}

/// Poll `Stats` until `done` holds or the settle time passes.
fn wait_for(client: &Client, done: impl Fn(&api::Stats) -> bool) -> Result<api::Stats, String> {
    let deadline = sys::clock() + SETTLE_TICKS;
    loop {
        let stats = client.stats().map_err(fail("stats"))?;
        if done(&stats) || sys::clock() >= deadline {
            return Ok(stats);
        }
        nap();
    }
}

/// Start `role=intruder` as a child, told the owner's ring id, and wait for it.
fn run_intruder(ring: u32) -> Result<bool, String> {
    let command = format!("NICCTL.ELF role=intruder ring={ring}\0");
    let Some(pid) = sys::spawn(command.as_bytes()) else {
        return Err(String::from(
            "could not start the intruder task (NICCTL.ELF missing?)",
        ));
    };
    let deadline = sys::clock() + 2 * SETTLE_TICKS;
    loop {
        if let Some((child, status)) = sys::wait(deadline) {
            if child == pid {
                return Ok(status == 0);
            }
        } else {
            return Err(String::from("the intruder task did not finish"));
        }
    }
}

/// The second task: every call that would change or use the owner's ring must
/// be refused, and the read-only calls must still work.
pub(super) fn run_intruder_role(ring: u32) -> Result<u32, String> {
    let client = connect()?;
    let mut checks = Checks { done: 0 };
    checks.refused(
        "DetachRing by a stranger",
        client.detach(ring),
        errno::EACCES,
    )?;
    checks.refused(
        "SetRxMode by a stranger",
        client.set_rx_mode(1),
        errno::EACCES,
    )?;
    checks.refused(
        "AttachRing by a stranger",
        Attempt::good(16).send(&client),
        errno::EBUSY,
    )?;
    // One-way calls cannot fail to the caller; the driver must ignore them.
    client.kick(ring).map_err(fail("kick"))?;
    client.kick(ring.wrapping_add(7)).map_err(fail("kick"))?;
    let info = client.info().map_err(fail("info"))?;
    checks.expect("Info works for anyone", info.mac.len() == 6)?;
    client.stats().map_err(fail("stats"))?;
    checks.expect("Stats works for anyone", true)?;
    Ok(checks.done)
}
