use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::{
    decode_event, events_wire, lucide, menu_row, pixels, wire, Event, Item, Object, Rect,
    Transport, Tray,
};

/// A shell in memory: records calls, refuses while `down`.
#[derive(Default)]
struct Fake {
    next: u64,
    calls: Vec<(u32, Vec<u8>, Vec<Object>)>,
    closed: Vec<u64>,
    down: bool,
    refuse: Option<i64>,
}

impl Transport for Fake {
    fn create_pair(&mut self) -> crate::Result<(u64, u64)> {
        self.next += 2;
        Ok((self.next, self.next + 1))
    }

    fn call(&mut self, method: u32, body: Vec<u8>, objects: Vec<Object>) -> crate::Result<Vec<u8>> {
        if self.down {
            return Err(-2);
        }
        if let Some(code) = self.refuse {
            return Err(code);
        }
        self.calls.push((method, body, objects));
        Ok(Vec::new())
    }

    fn close(&mut self, handle: u64) {
        self.closed.push(handle);
    }
}

fn item() -> Item {
    let mut item = crate::item(lucide("volume-2"), "Volume");
    item.menu = vec![menu_row(1, "Mute", wire::MENU_KIND_CHECK)];
    item
}

fn sets(tray: &mut Tray<Fake>) -> Vec<wire::Item> {
    tray.transport_mut()
        .calls
        .iter()
        .filter(|(method, _, _)| *method == wire::METHOD_SET)
        .map(|(_, body, objects)| wire::decode_set_args(body, objects).unwrap().item)
        .collect()
}

#[test]
fn set_transfers_one_end_and_keeps_the_other() {
    let mut tray = Tray::new(Fake::default());
    tray.set(item()).unwrap();
    let (method, _, objects) = tray.transport_mut().calls[0].clone();
    assert_eq!(method, wire::METHOD_SET);
    assert_eq!(objects, vec![Object::Channel(2)]);
    assert_eq!(tray.events(), Some(3));
    // A second Set replaces the channel and closes the old end.
    tray.set(item()).unwrap();
    assert_eq!(tray.events(), Some(5));
    assert_eq!(tray.transport_mut().closed, vec![3]);
}

#[test]
fn a_new_generation_sets_the_item_again_with_its_updates() {
    let mut tray = Tray::new(Fake::default());
    assert!(tray.generation(7).is_none(), "nothing to set yet");
    tray.set(item()).unwrap();
    assert!(tray.generation(7).is_none(), "already registered with 7");
    let patch = wire::UpdateArgs {
        tooltip: Some("Volume 80%".into()),
        badge: Some(String::new()),
        ..wire::UpdateArgs::default()
    };
    tray.update(patch).unwrap();
    assert!(matches!(tray.generation(8), Some(Ok(()))));
    let sets = sets(&mut tray);
    assert_eq!(sets.len(), 2);
    assert_eq!(sets[1].tooltip, "Volume 80%");
    assert_eq!(sets[1].badge, None);
    assert!(tray.generation(8).is_none());
}

#[test]
fn a_set_without_a_shell_is_retried_on_the_next_generation() {
    let mut tray = Tray::new(Fake {
        down: true,
        ..Fake::default()
    });
    assert_eq!(tray.set(item()), Err(-2));
    assert_eq!(tray.events(), None);
    assert_eq!(tray.transport_mut().closed, vec![2, 3]);
    tray.transport_mut().down = false;
    assert!(matches!(tray.generation(1), Some(Ok(()))));
    assert_eq!(tray.events(), Some(5));
    // A new generation whose shell refuses at first is retried.
    tray.transport_mut().down = true;
    assert!(matches!(tray.generation(2), Some(Err(_))));
    tray.transport_mut().down = false;
    assert!(matches!(tray.retry(), Some(Ok(()))));
    assert!(tray.retry().is_none());
    // Not found in the shell's copy of init's table yet (ESRCH): retried.
    tray.transport_mut().refuse = Some(-3);
    assert!(matches!(tray.generation(5), Some(Err(-3))));
    tray.transport_mut().refuse = None;
    assert!(matches!(tray.retry(), Some(Ok(()))));
    // A refusal (here EACCES) waits for the next generation.
    tray.transport_mut().refuse = Some(-13);
    assert!(matches!(tray.generation(3), Some(Err(-13))));
    assert!(tray.retry().is_none());
    tray.transport_mut().refuse = None;
    assert!(matches!(tray.generation(4), Some(Ok(()))));
}

#[test]
fn clear_stops_re_registration() {
    let mut tray = Tray::new(Fake::default());
    tray.set(item()).unwrap();
    tray.clear().unwrap();
    assert_eq!(tray.events(), None);
    assert!(tray.generation(9).is_none());
    tray.set(item()).unwrap();
    tray.disconnected();
    assert!(matches!(tray.generation(9), Some(Ok(()))));
}

#[test]
fn events_decode_and_foreign_or_broken_ones_do_not() {
    let anchor = events_wire::Rect {
        x: 1000,
        y: 690,
        w: 24,
        h: 24,
    };
    let body = events_wire::encode_activate_args(&events_wire::ActivateArgs {
        anchor: anchor.clone(),
        popup: 5,
    })
    .unwrap();
    let id = events_wire::INTERFACE_ID;
    assert_eq!(
        decode_event(id, events_wire::METHOD_ACTIVATE, &body),
        Some(Event::Activate {
            anchor: Rect {
                x: 1000,
                y: 690,
                w: 24,
                h: 24
            },
            popup: 5
        })
    );
    let scroll = events_wire::encode_scroll_args(&events_wire::ScrollArgs { delta: -2 }).unwrap();
    assert_eq!(
        decode_event(id, events_wire::METHOD_SCROLL, &scroll),
        Some(Event::Scroll { delta: -2 })
    );
    assert_eq!(
        decode_event(id, events_wire::METHOD_PING, &[]),
        Some(Event::Ping)
    );
    assert_eq!(decode_event(id ^ 1, events_wire::METHOD_PING, &[]), None);
    assert_eq!(decode_event(id, 99, &[]), None);
    assert_eq!(
        decode_event(id, events_wire::METHOD_ACTIVATE, &body[..3]),
        None
    );
}

#[test]
fn seeded_garbage_never_panics_the_decoder() {
    fuzzkit::for_seeds("seeded_garbage_never_panics_the_decoder", |_, rng| {
        let method = rng.range(0, 7) as u32;
        let len = rng.below(64) as usize;
        let body = rng.bytes(len);
        let _ = decode_event(events_wire::INTERFACE_ID, method, &body);
    });
}

#[test]
fn pixels_builds_one_image_per_scale() {
    let icon = pixels(vec![(1, 1, vec![0; 4]), (2, 2, vec![0; 16])]);
    assert_eq!(icon.pixels.len(), 2);
    assert_eq!(icon.pixels[1].width, 2);
    assert!(icon.lucide.is_none());
}
