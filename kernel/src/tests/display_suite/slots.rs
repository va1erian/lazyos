//! The display v1 buffer-slot rules behind the pipelined `Present` (issue
//! #361): `surfbuf`'s compositor-side `SlotTable`, damage clipping, and the
//! client-side `Swapchain`. `xuid` and its clients link the same crate, so
//! these tests cover the logic that runs in ring 3, with a compositor and a
//! client simulated back to back: correctness (slot bounds, `EBUSY`, damage
//! rules, release-before-`FrameDone` ordering) and a soak that replaces
//! buffers as fast as the model allows while checking nothing ever tears.

use super::*;
use alloc::collections::VecDeque;
use surfbuf::{
    clip_damage, Area, AttachError, PresentError, SlotTable, Swapchain, MAX_DAMAGE, MAX_SLOTS,
};

/// A compositor-to-client event, as `xuid` would send it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ev {
    Release(u32),
    Done(u64),
}

/// A table with every slot attached; the payload is the slot number.
fn full_table() -> Result<SlotTable<u32>, String> {
    let mut table = SlotTable::new();
    for slot in 0..MAX_SLOTS as u32 {
        check!(
            table.attach(slot, slot) == Ok(None),
            "fresh attach of slot {slot}"
        );
    }
    Ok(table)
}

/// Slot ids are bounded, the current slot cannot be replaced, others can.
pub fn slots_attach_rules() -> Result<(), String> {
    let mut table = SlotTable::new();
    check!(
        table.attach(MAX_SLOTS as u32, 0u32) == Err(AttachError::BadSlot),
        "slot 4"
    );
    check!(
        table.attach(u32::MAX, 0u32) == Err(AttachError::BadSlot),
        "slot u32::MAX"
    );
    check!(table.attach(0, 10) == Ok(None), "first attach");
    check!(
        table.attach(0, 11) == Ok(Some(10)),
        "replace a non-current slot"
    );
    check!(table.present(0) == Ok(None), "present slot 0");
    check!(
        table.attach(0, 12) == Err(AttachError::Busy),
        "current slot must be Busy"
    );
    check!(
        table.current() == Some(&11),
        "the refused attach kept the old buffer"
    );
    check!(
        table.attach(1, 13) == Ok(None),
        "another slot stays attachable"
    );
    // The legacy attach replaces slot 0 immediately and is not pipelined.
    let mut legacy = SlotTable::new();
    check!(
        legacy.attach_legacy(1u32) == Ok(None),
        "legacy first attach"
    );
    check!(
        legacy.attach_legacy(2) == Ok(Some(1)),
        "legacy replace returns the old mapping"
    );
    check!(
        legacy.current() == Some(&2) && !legacy.is_pipelined(),
        "legacy stays unpipelined"
    );
    // Once a surface has presented, a legacy attach is refused and leaves the
    // client's slot ownership alone.
    let mut piped = SlotTable::new();
    piped.attach(1, 7u32).map_err(|_| "attach slot 1")?;
    piped.present(1).map_err(|_| "present slot 1")?;
    check!(
        piped.attach_legacy(9) == Err(AttachError::Busy),
        "legacy attach refused after Present"
    );
    check!(
        piped.current_slot() == Some(1),
        "the refused legacy attach kept slot 1 current"
    );
    Ok(())
}

/// `present` swaps the current slot and names the one to release, refuses
/// bad or empty slots without changing anything, and `take_all` frees all.
pub fn slots_present_and_release() -> Result<(), String> {
    let mut table = SlotTable::new();
    check!(
        table.present(0) == Err(PresentError::BadSlot),
        "present of an empty slot"
    );
    table.attach(0, 0u32).map_err(|e| format!("{e:?}"))?;
    table.attach(1, 1).map_err(|e| format!("{e:?}"))?;
    check!(
        table.present(0) == Ok(None),
        "first present releases nothing"
    );
    check!(table.is_pipelined(), "Present marks the surface pipelined");
    check!(
        table.present(0) == Ok(None),
        "re-presenting the current slot releases nothing"
    );
    check!(
        table.present(1) == Ok(Some(0)),
        "swap releases the old slot"
    );
    check!(
        table.present(2) == Err(PresentError::BadSlot),
        "unattached slot"
    );
    check!(
        table.present(99) == Err(PresentError::BadSlot),
        "out-of-range slot"
    );
    check!(
        table.current_slot() == Some(1),
        "refused presents change nothing"
    );
    let all = table.take_all();
    check!(
        all.iter().flatten().count() == 2,
        "take_all returns both mappings"
    );
    check!(table.current().is_none(), "nothing current after take_all");
    Ok(())
}

/// The damage rules: empty or too many rects is the whole surface, others are
/// clipped, and hostile sums cannot wrap.
pub fn slots_damage_clipping() -> Result<(), String> {
    let clip = |rects: &[Area]| -> Vec<Area> {
        let (out, count) = clip_damage(64, 32, rects.iter().copied());
        out[..count].to_vec()
    };
    let whole = Area {
        x: 0,
        y: 0,
        w: 64,
        h: 32,
    };
    check!(clip(&[]) == [whole], "empty list is the whole surface");
    let many = vec![
        Area {
            x: 1,
            y: 1,
            w: 1,
            h: 1
        };
        MAX_DAMAGE + 1
    ];
    check!(
        clip(&many) == [whole],
        "too many rects is the whole surface"
    );
    let hostile = [
        Area {
            x: u32::MAX,
            y: 0,
            w: u32::MAX,
            h: 1,
        },
        Area {
            x: 60,
            y: 30,
            w: u32::MAX,
            h: u32::MAX,
        },
        Area {
            x: 0,
            y: 0,
            w: 0,
            h: 9,
        },
        Area {
            x: 64,
            y: 0,
            w: 1,
            h: 1,
        },
    ];
    check!(
        clip(&hostile)
            == [Area {
                x: 60,
                y: 30,
                w: 4,
                h: 2
            }],
        "hostile rects: {:?}",
        clip(&hostile)
    );
    Ok(())
}

/// Two slots, in order: the release of the replaced buffer precedes the
/// `FrameDone` of the present that replaced it, and the chain paces on it.
pub fn slots_double_buffer_ordering() -> Result<(), String> {
    let mut table = full_table()?;
    let mut chain = Swapchain::new(2);
    let mut events: VecDeque<Ev> = VecDeque::new();
    let mut present = |chain: &mut Swapchain, table: &mut SlotTable<u32>| -> Result<u32, String> {
        let slot = chain.acquire().ok_or("no free slot")?;
        let seq = chain.submit(slot).ok_or("submit refused")?;
        let release = table.present(slot).map_err(|e| format!("{e:?}"))?;
        if let Some(old) = release {
            events.push_back(Ev::Release(old));
        }
        events.push_back(Ev::Done(seq));
        Ok(slot)
    };
    let a = present(&mut chain, &mut table)?;
    let b = present(&mut chain, &mut table)?;
    check!(a != b, "the second frame must use the other buffer");
    check!(
        chain.acquire().is_none(),
        "both buffers are held until a release"
    );
    check!(
        events == [Ev::Done(1), Ev::Release(a), Ev::Done(2)],
        "event order: {events:?}"
    );
    while let Some(event) = events.pop_front() {
        match event {
            Ev::Release(slot) => check!(chain.released(slot), "release of {slot} refused"),
            Ev::Done(seq) => check!(chain.frame_done(seq), "frame {seq} refused"),
        }
    }
    check!(
        chain.acquire() == Some(a),
        "the released buffer is drawable again"
    );
    check!(chain.in_flight() == 0, "all frames completed");
    // Stray or replayed events are refused.
    check!(!chain.released(a), "a slot we already own");
    check!(!chain.frame_done(2), "a replayed FrameDone");
    check!(!chain.frame_done(9), "a FrameDone from the future");
    Ok(())
}

/// A tiny deterministic generator so a failure reproduces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// `DetachBufferSlot` (issue #372): a non-current slot empties and can be
/// attached again; the current slot and out-of-range ids are refused and
/// leave the table unchanged.
pub fn slots_detach_rules() -> Result<(), String> {
    let mut table = full_table()?;
    check!(
        table.detach(MAX_SLOTS as u32) == Err(AttachError::BadSlot),
        "out-of-range detach"
    );
    table.present(2).map_err(|_| "present slot 2")?;
    check!(
        table.detach(2) == Err(AttachError::Busy),
        "the current slot must be Busy"
    );
    check!(table.current() == Some(&2), "a refused detach kept the screen");
    check!(table.detach(1) == Ok(Some(1)), "detach returns the mapping");
    check!(table.detach(1) == Ok(None), "an empty slot detaches to nothing");
    check!(
        table.present(1) == Err(PresentError::BadSlot),
        "a detached slot cannot be presented"
    );
    check!(table.attach(1, 9) == Ok(None), "a detached slot reattaches");
    check!(table.present(1) == Ok(Some(2)), "and presents again");
    Ok(())
}

/// Client and compositor exchange hundreds of thousands of frames with the
/// compositor lagging behind a random amount and hostile calls mixed in. The
/// "memory" of each slot is a generation tag: the client only writes slots the
/// chain hands it, and the compositor checks the current slot's tag never
/// changes under it (a tear) and matches what was presented.
pub fn slots_present_soak() -> Result<(), String> {
    const STEPS: u32 = 400_000;
    for slots in 2..=MAX_SLOTS {
        let mut table = full_table()?;
        let mut chain = Swapchain::new(slots);
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ slots as u64);
        let mut memory = [0u64; MAX_SLOTS];
        let mut generation = 0u64;
        // Presents sent but not yet processed: (slot, seq, tag written).
        let mut pending: VecDeque<(u32, u64, u64)> = VecDeque::new();
        let mut events: VecDeque<Ev> = VecDeque::new();
        // Per seq: did that present replace a buffer (so a release precedes).
        let mut replaced: Vec<bool> = vec![false];
        let mut shown_tag = None::<u64>;
        let (mut presented, mut done, mut released) = (0u64, 0u64, 0u64);
        let mut release_seen = false;

        for _ in 0..STEPS {
            match rng.next() % 6 {
                // Client draws into a free slot and presents it.
                0 | 1 => {
                    if let Some(slot) = chain.acquire() {
                        check!(
                            table.current_slot() != Some(slot)
                                && !pending.iter().any(|p| p.0 == slot),
                            "client handed slot {slot} the compositor may read"
                        );
                        // Now and then the client reallocates the slot (a
                        // resize): detach, then attach a fresh buffer. A free
                        // slot is never current, so neither is refused.
                        if rng.next() % 8 == 0 {
                            check!(table.detach(slot).is_ok(), "detach free slot {slot}");
                            check!(
                                table.attach(slot, slot) == Ok(None),
                                "reattach detached slot {slot}"
                            );
                        }
                        generation += 1;
                        memory[slot as usize] = generation;
                        let seq = chain.submit(slot).ok_or("submit refused a free slot")?;
                        pending.push_back((slot, seq, generation));
                    }
                }
                // Compositor takes the oldest present.
                2 | 3 => {
                    if let Some((slot, seq, tag)) = pending.pop_front() {
                        let release = table
                            .present(slot)
                            .map_err(|e| format!("present({slot}): {e:?}"))?;
                        check!(
                            memory[slot as usize] == tag,
                            "slot {slot} changed between submit and present"
                        );
                        shown_tag = Some(tag);
                        replaced.push(release.is_some());
                        if let Some(old) = release {
                            events.push_back(Ev::Release(old));
                        }
                        events.push_back(Ev::Done(seq));
                        presented += 1;
                    }
                }
                // Client consumes one event.
                4 => match events.pop_front() {
                    Some(Ev::Release(slot)) => {
                        check!(chain.released(slot), "release of unheld slot {slot}");
                        release_seen = true;
                        released += 1;
                    }
                    Some(Ev::Done(seq)) => {
                        check!(chain.frame_done(seq), "out-of-order FrameDone {seq}");
                        check!(
                            !replaced[seq as usize] || release_seen,
                            "FrameDone({seq}) arrived before its BufferRelease"
                        );
                        release_seen = false;
                        done += 1;
                    }
                    None => {}
                },
                // Hostile / repaint traffic: refused calls change nothing and
                // the buffer on screen is stable.
                _ => {
                    if let Some(current) = table.current_slot() {
                        check!(
                            table.attach(current, current) == Err(AttachError::Busy),
                            "attach into the current slot must be Busy"
                        );
                        check!(
                            table.detach(current) == Err(AttachError::Busy),
                            "detach of the current slot must be Busy"
                        );
                        check!(
                            memory[current as usize] == shown_tag.unwrap_or(0),
                            "the displayed buffer changed under the compositor"
                        );
                    }
                    check!(
                        table
                            .present(MAX_SLOTS as u32 + (rng.next() % 5) as u32)
                            .is_err(),
                        "out-of-range present accepted"
                    );
                }
            }
        }
        // Drain and balance the books.
        while let Some((slot, seq, _)) = pending.pop_front() {
            let release = table.present(slot).map_err(|e| format!("drain: {e:?}"))?;
            replaced.push(release.is_some());
            if let Some(old) = release {
                events.push_back(Ev::Release(old));
            }
            events.push_back(Ev::Done(seq));
            presented += 1;
        }
        while let Some(event) = events.pop_front() {
            match event {
                Ev::Release(slot) => {
                    check!(chain.released(slot), "final release {slot}");
                    release_seen = true;
                    released += 1;
                }
                Ev::Done(seq) => {
                    check!(chain.frame_done(seq), "final FrameDone {seq}");
                    check!(!replaced[seq as usize] || release_seen, "final ordering");
                    release_seen = false;
                    done += 1;
                }
            }
        }
        check!(
            presented == done && chain.in_flight() == 0,
            "frames lost: {presented}/{done}"
        );
        // Every present but the first (and re-presents of the same slot, which
        // cannot happen with a chain) released exactly one buffer.
        check!(
            released + 1 == presented,
            "releases {released} for {presented} presents"
        );
        check!(presented > 10_000, "soak too small: {presented} presents");
    }
    Ok(())
}
