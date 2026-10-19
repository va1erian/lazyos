//! Host tests of compositing: what repaints, what is kept, where it lands.

use std::cell::Cell;
use std::rc::Rc;

use xui_core::backend::{Canvas, Event, ParentRef, WidgetId};
use xui_core::router::WidgetHost;
use xui_core::Color;

use super::super::test_support::client_backend;
use super::super::{Node, Window};
use super::*;

const W: WindowId = WindowId::from_raw(1);
const RED: Color = Color::rgb(200, 0, 0);
const BLUE: Color = Color::rgb(0, 0, 200);

struct Sink;

impl WidgetHost for Sink {
    fn deliver(&self, _target: WidgetId, _event: &Event) -> bool {
        true
    }
}

/// A node at `bounds` whose painter fills it with `color` and counts its
/// runs in `calls`.
fn painted(bounds: Rect, color: Color, calls: Rc<Cell<u32>>) -> Node {
    let painter: Painter = Rc::new(move |canvas: &mut dyn Canvas| {
        calls.set(calls.get() + 1);
        canvas.fill_rect(canvas.bounds(), color);
    });
    Node {
        window: W,
        parent: ParentRef::Window(W),
        bounds,
        visible: true,
        enabled: true,
        focus_stop: false,
        wants_tab: false,
        popup: false,
        clip: None,
        text: String::new(),
        painter: Some(painter),
    }
}

/// The pixel at `(x, y)` of the window's surface.
fn pixel(backend: &LazyOSBackend, x: u32, y: u32) -> [u8; 4] {
    let windows = backend.windows.borrow();
    let at = ((y * 64 + x) * 4) as usize;
    windows[&W.raw()].frame[at..at + 4]
        .try_into()
        .expect("in bounds")
}

fn rgba(color: Color) -> [u8; 4] {
    [color.r, color.g, color.b, 255]
}

/// A 64x64 window with a red node on the left and a blue node on the
/// right, painted once in full; the per-node paint counters.
fn rig() -> (LazyOSBackend, Rc<Cell<u32>>, Rc<Cell<u32>>) {
    let backend = client_backend();
    backend
        .windows
        .borrow_mut()
        .insert(W.raw(), Window::for_tests(Rc::new(Sink)));
    let (left, right) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
    let mut nodes = backend.nodes.borrow_mut();
    nodes.push((
        WidgetId::from_raw(1),
        painted(Rect::new(0, 0, 32, 64), RED, left.clone()),
    ));
    nodes.push((
        WidgetId::from_raw(2),
        painted(Rect::new(32, 0, 64, 64), BLUE, right.clone()),
    ));
    drop(nodes);
    assert!(backend.composite(W, Rect::new(0, 0, 64, 64), Dest::Frame));
    assert_eq!((left.get(), right.get()), (1, 1));
    (backend, left, right)
}

#[test]
fn hiding_a_node_repaints_what_its_painter_spilled() {
    use xui_core::backend::Backend;

    let (backend, _, _) = rig();
    backend.take_damage(W, Rect::new(0, 0, 64, 64));
    backend.set_visible(WidgetId::from_raw(1), false);
    // The node is 0..32 wide; the repaint reaches PAINT_SPILL past it.
    let damage = backend.take_damage(W, Rect::new(0, 0, 64, 64));
    assert_eq!(damage, Rect::new(0, 0, 32 + PAINT_SPILL, 64));
}

#[test]
fn a_painter_outside_the_damage_is_not_run() {
    let (backend, left, right) = rig();
    assert!(backend.composite(W, Rect::new(4, 4, 20, 20), Dest::Frame));
    assert_eq!(left.get(), 2, "the damaged node repainted");
    assert_eq!(right.get(), 1, "the undamaged node did not");
}

#[test]
fn pixels_outside_the_damage_are_untouched() {
    let (backend, _, _) = rig();
    // Recolour the left node, then repaint only part of it.
    backend.nodes.borrow_mut()[0].1 = painted(Rect::new(0, 0, 32, 64), BLUE, Rc::default());
    assert!(backend.composite(W, Rect::new(4, 4, 20, 20), Dest::Frame));
    assert_eq!(pixel(&backend, 10, 10), rgba(BLUE), "inside the damage");
    assert_eq!(pixel(&backend, 2, 2), rgba(RED), "outside: the old frame");
    assert_eq!(pixel(&backend, 25, 30), rgba(RED), "outside: the old frame");
}

#[test]
fn a_painter_bigger_than_the_damage_rasterises_only_the_damage() {
    let (backend, _, _) = rig();
    backend.nodes.borrow_mut()[0].1 = painted(Rect::new(0, 0, 32, 64), BLUE, Rc::default());
    assert!(backend.composite(W, Rect::new(4, 4, 20, 20), Dest::Frame));
    // The painting surface itself, not only the frame, kept the rest.
    let windows = backend.windows.borrow();
    let surface = windows[&W.raw()].surface.pixels();
    let at = |x: usize, y: usize| &surface[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
    assert_eq!(at(10, 10), rgba(BLUE), "inside the damage");
    assert_eq!(at(2, 2), rgba(RED), "outside: never painted");
    assert_eq!(at(25, 30), rgba(RED), "outside: never painted");
}

#[test]
fn a_painter_straddling_the_damage_cannot_overdraw_its_neighbour() {
    let (backend, left, right) = rig();
    // Make the left node overlap the right one; only a strip clear of the
    // right node is damaged, so the right node's pixels must survive the
    // left one painting over them on the surface.
    backend.nodes.borrow_mut()[0].1 = painted(Rect::new(0, 0, 48, 64), RED, left.clone());
    assert!(backend.composite(W, Rect::new(0, 0, 24, 64), Dest::Frame));
    assert_eq!(right.get(), 1);
    assert_eq!(pixel(&backend, 40, 10), rgba(BLUE));
    assert_eq!(pixel(&backend, 20, 10), rgba(RED));
    assert_eq!(pixel(&backend, 28, 10), rgba(RED), "outside: the old frame");
}

#[test]
fn a_node_outside_its_ancestors_clip_is_not_painted() {
    let (backend, left, _) = rig();
    // The right node clips its children to its left half: a child wholly
    // in the right half is a row scrolled out of a viewport.
    let row = Rc::new(Cell::new(0));
    {
        let mut nodes = backend.nodes.borrow_mut();
        nodes[1].1.clip = Some(Rect::new(0, 0, 16, 64));
        let mut child = painted(Rect::new(20, 0, 32, 64), RED, row.clone());
        child.parent = ParentRef::Widget(WidgetId::from_raw(2));
        nodes.push((WidgetId::from_raw(3), child));
    }
    assert!(backend.composite(W, Rect::new(0, 0, 64, 64), Dest::Frame));
    assert_eq!(row.get(), 0, "the clipped-out row did not paint");
    assert_eq!(left.get(), 2);
    assert_eq!(pixel(&backend, 58, 10), rgba(BLUE));
}

#[test]
fn a_node_within_the_spill_margin_repaints_with_the_damage() {
    let (backend, _, right) = rig();
    // The right node starts at x = 32, one pixel past this damage.
    assert!(backend.composite(W, Rect::new(10, 10, 31, 20), Dest::Frame));
    assert_eq!(right.get(), 2);
}

#[test]
fn the_damage_is_cleared_to_the_background_first() {
    let (backend, _, _) = rig();
    backend.nodes.borrow_mut().remove(0);
    assert!(backend.composite(W, Rect::new(0, 0, 32, 64), Dest::Frame));
    let background = backend.windows.borrow()[&W.raw()].theme.background;
    assert_eq!(pixel(&backend, 10, 10), rgba(background));
    assert_eq!(pixel(&backend, 40, 10), rgba(BLUE));
}

/// The two buffers behind a test window's slots.
struct Buffers([*const u8; 2]);

impl Buffers {
    /// Slot `slot`'s pixels, read between composites.
    fn slot(&self, slot: usize) -> &[u8] {
        // SAFETY: the buffers are leaked 64x64 RGBA images, alive for the
        // whole test, and nothing writes them while this borrow is held.
        unsafe { core::slice::from_raw_parts(self.0[slot], 64 * 64 * 4) }
    }

    fn pixel(&self, slot: usize, x: usize, y: usize) -> [u8; 4] {
        let at = (y * 64 + x) * 4;
        self.slot(slot)[at..at + 4].try_into().expect("in bounds")
    }
}

/// [`rig`] with `W` made a client window with two 64x64 slots.
fn with_slots() -> (LazyOSBackend, Buffers) {
    let (backend, _, _) = rig();
    let leak = || Box::leak(vec![0u8; 64 * 64 * 4].into_boxed_slice());
    let (a, b) = (leak(), leak());
    let mut client = ClientWindow::for_tests(1);
    client.slots = Slots::for_tests([a, b], 64, 64);
    // Read through the addresses the slots write through.
    let address = |slot| client.slots.mapping(slot, 64, 64).expect("64x64").0 as *const u8;
    let buffers = Buffers([address(0), address(1)]);
    backend
        .windows
        .borrow_mut()
        .get_mut(&W.raw())
        .expect("rigged")
        .client = Some(client);
    (backend, buffers)
}

/// One client frame without the `Present`: prepare `slot`, compose into it.
fn compose_slot(backend: &LazyOSBackend, slot: u32, damage: Rect) -> Rect {
    let compose = backend
        .with_slots(W, |slots| slots.prepare(slot, damage))
        .flatten()
        .expect("a prepared slot");
    assert!(backend.composite(W, compose, Dest::Slot(slot)));
    compose
}

fn frame_pixels(backend: &LazyOSBackend) -> Vec<u8> {
    backend.windows.borrow()[&W.raw()].frame.clone()
}

#[test]
fn a_slot_composite_writes_what_the_frame_would_hold() {
    let (backend, buffers) = with_slots();
    let full = Rect::new(0, 0, 64, 64);
    assert_eq!(compose_slot(&backend, 0, Rect::new(3, 3, 9, 9)), full);
    assert_eq!(buffers.slot(0), frame_pixels(&backend).as_slice());
}

#[test]
fn a_slot_takes_what_it_missed_from_the_slot_composed_last() {
    let (backend, buffers) = with_slots();
    let full = Rect::new(0, 0, 64, 64);
    compose_slot(&backend, 0, full);
    compose_slot(&backend, 1, full);
    // Frame 1 recolours part of the left node into slot 0.
    backend.nodes.borrow_mut()[0].1 = painted(Rect::new(0, 0, 32, 64), BLUE, Rc::default());
    let first = Rect::new(4, 4, 20, 20);
    assert_eq!(compose_slot(&backend, 0, first), first);
    // Frame 2 damages elsewhere and lands in slot 1, which must also show
    // frame 1's change, copied from slot 0 rather than recomposed.
    let second = Rect::new(40, 40, 50, 50);
    assert_eq!(compose_slot(&backend, 1, second), second);
    // The frame path, composing the same damage, is the reference picture.
    assert!(backend.composite(W, first, Dest::Frame));
    assert!(backend.composite(W, second, Dest::Frame));
    assert_eq!(buffers.slot(1), frame_pixels(&backend).as_slice());
    assert_eq!(buffers.pixel(1, 10, 10), rgba(BLUE));
    assert_eq!(buffers.pixel(1, 2, 2), rgba(RED));
}

/// Gives the right node a direct painter that fills its damage with green,
/// or declines when `draws` is `false`.
fn green_direct_painter(draws: bool) -> WidgetId {
    let id = WidgetId::from_raw(2);
    super::super::direct::set_painter(
        id,
        Some(Rc::new(
            move |target: &mut super::super::direct::Target<'_>| {
                if !draws {
                    return false;
                }
                let d = target.damage;
                for y in d.top..d.bottom {
                    let row = y as usize * target.stride;
                    for x in d.left..d.right {
                        let at = row + x as usize * 4;
                        target.pixels[at..at + 4].copy_from_slice(&[0, 200, 0, 255]);
                    }
                }
                true
            },
        )),
    );
    id
}

#[test]
fn a_direct_painter_writes_into_the_slot() {
    let (backend, buffers) = with_slots();
    let full = Rect::new(0, 0, 64, 64);
    compose_slot(&backend, 0, full);
    compose_slot(&backend, 1, full);
    let id = green_direct_painter(true);
    let damage = Rect::new(24, 8, 48, 16);
    compose_slot(&backend, 0, damage);
    super::super::direct::set_painter(id, None);
    assert_eq!(buffers.pixel(0, 40, 10), [0, 200, 0, 255]);
    assert_eq!(buffers.pixel(0, 26, 10), rgba(RED), "the canvas around it");
    assert_eq!(buffers.pixel(0, 40, 20), rgba(BLUE), "outside the damage");
}

#[test]
fn a_declining_direct_painter_leaves_the_damage_to_the_canvas() {
    let (backend, buffers) = with_slots();
    // A fresh slot holds zeros: a painter that wrote nothing would show them.
    let id = green_direct_painter(false);
    compose_slot(&backend, 0, Rect::new(0, 0, 64, 64));
    super::super::direct::set_painter(id, None);
    assert_eq!(
        buffers.pixel(0, 40, 10),
        rgba(BLUE),
        "the canvas painter's picture"
    );
    assert_eq!(buffers.pixel(0, 10, 10), rgba(RED));
}

#[test]
fn a_window_closed_while_its_slot_is_painted_keeps_its_buffers() {
    use xui_core::backend::Backend;

    let (backend, _) = with_slots();
    backend.composing.set(Some(W.raw()));
    backend.close_window(W);
    let kept = backend.closing.borrow_mut().take();
    assert!(kept.is_some(), "closed once the painter returns");
    // Host tests have no buffers to release.
    std::mem::forget(kept);
}
