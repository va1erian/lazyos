//! `dragdemo` (`DRAGDMO.ELF`): the compositor-mediated drag & drop demo pair
//! (issue #145).
//!
//! The kernel boots this program next to `xuid` with no manifest argument, so
//! it is the **launcher**: it starts a `source` and a `target` child of itself
//! (two separate display clients in one clipboard session) and reaps them.
//!
//! * the **source** offers a typed payload through `clipboardd`
//!   (`text/x-dnd-demo`), draws a coloured item, and when a press travels past
//!   [`DRAG_THRESHOLD`] calls `DragStart(surface, token, mime)`; the
//!   compositor owns the pointer from there;
//! * the **target** receives `DragEnter`/`DragLeave`/`Drop` from the
//!   compositor and, on a drop, pastes the delivered token back through
//!   `clipboardd` — the same authorization path as any paste — then checks the
//!   bytes and shows their type and length.
//!
//! The cross-session denial probe runs in a short-lived `probe` child of the
//! target, so switching into the probe session never changes the target's own
//! credentials: every later drop in the same boot still pastes normally.
//!
//! Evidence markers: `DND:START:PASS` (the compositor accepted the drag),
//! `DND:DROP:PASS` (the target pasted the token), `DND:CANCEL:PASS` (Escape
//! cancelled the drag) and `DND:DENIED:PASS` (a cross-session paste of the
//! dropped token is refused). Failures print `DND:<name>:FAIL:<detail>`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::messenger::display::{self, Canvas, Client, Color, DragKind, Event, EventKind, Rect};
use user::messenger::{self, clipboard, errno, Endpoint, Error};
use user::sys;

/// Surface content size in pixels.
const W: i32 = 220;
const H: i32 = 180;
/// MIME type the source offers and the target expects.
const DEMO_MIME: &str = "text/x-dnd-demo";
/// The payload; the target checks the bytes it pastes against this.
const PAYLOAD: &[u8] = b"dnd payload #42";
/// Pointer travel (surface pixels) before a press becomes a drag.
const DRAG_THRESHOLD: i64 = 6;
/// PIT ticks the target waits for its denial-probe child to exit.
const PROBE_TICKS: u64 = 500;
/// PIT ticks `connect` retries while the compositor/services settle.
const CONNECT_ATTEMPTS: usize = 200;
/// The session the denial probe switches to (mirrors `clippaste`).
const PROBE_SESSION: u64 = 4242;
/// The drag source's item rectangle inside its content.
const ITEM: Rect = Rect::new(24, 52, 56, 56);

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match role() {
        Role::Launcher => launcher(),
        Role::Source => source(),
        Role::Target => target(),
        Role::Probe { token, mime } => probe(token, &mime),
    }
}

/// The role the launcher passed in the kernel's service argument string.
enum Role {
    Launcher,
    Source,
    Target,
    /// `probe <token> <mime>`: the target's one-shot denial-probe child.
    Probe {
        token: u64,
        mime: String,
    },
}

/// Read this task's manifest argument (`""` for a kernel-spawned launcher,
/// `source`/`target` for the children, `probe <token> <mime>` for the
/// target's denial-probe child).
fn role() -> Role {
    let mut buffer = [0u8; 32 + display::MAX_MIME];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let args = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    let mut words = args.split(' ');
    match words.next().unwrap_or("") {
        "source" => Role::Source,
        "target" => Role::Target,
        "probe" => {
            let token = words.next().and_then(|word| word.parse().ok()).unwrap_or(0);
            let mime = String::from(words.next().unwrap_or(""));
            Role::Probe { token, mime }
        }
        _ => Role::Launcher,
    }
}

/// Start both display clients as children (so the clipboard session scope sees
/// one session) and reap them for the life of the boot.
fn launcher() -> ! {
    sys::write_str("dragdemo: launcher (issue #145)\n");
    ensure_clipboard();
    let mut started = 0u64;
    for role in ["source", "target"] {
        let command = format!("DRAGDMO.ELF {role}\0");
        match sys::spawn(command.as_bytes()) {
            Some(pid) => {
                started += 1;
                sys::write_str(&format!("dragdemo: started {role} (pid {pid})\n"));
            }
            None => sys::write_str(&format!("DND:LAUNCH:FAIL:{role}\n")),
        }
    }
    if started == 2 {
        sys::write_str("DND:LAUNCH:PASS\n");
    }
    loop {
        let _ = sys::wait(sys::clock() + 10);
    }
}

/// Start `clipboardd` when nothing serves the clipboard yet: the plain
/// `LAZYOS_XUID=1` boot has no supervisor, and the drag's token transfer needs
/// the service. In services mode `init` already runs it, so this is a no-op.
fn ensure_clipboard() {
    for _ in 0..CONNECT_ATTEMPTS {
        if clipboard::Client::connect().is_ok() {
            sys::write_str("dragdemo: clipboardd already present\n");
            return;
        }
        park_tick();
    }
    match sys::spawn(b"CLIPD.ELF\0") {
        Some(pid) => sys::write_str(&format!("dragdemo: started clipboardd (pid {pid})\n")),
        None => {
            sys::write_str("DND:LAUNCH:FAIL:clipboardd did not start\n");
        }
    }
}

/// A connected display client with its surface, event endpoint and pixel
/// buffer.
struct App {
    display: Client,
    surface: u64,
    events: Endpoint,
    canvas: Canvas,
    buffer: Vec<u8>,
}

/// Create the surface, allocate and attach its buffer, and commit the first
/// frame.
fn attach(display: Client, title: &str, draw: &dyn Fn(&mut Canvas)) -> App {
    let (events, events_server) = match messenger::create_pair() {
        Ok(pair) => pair,
        Err(_) => fatal("create_pair"),
    };
    let surface = match display.create_surface(W as u64, H as u64, title, &events_server) {
        Ok(surface) => surface,
        Err(_) => fatal("create_surface"),
    };
    let bytes = (W * H * 4) as u64;
    let (buffer, va) = match sys::display_create_buffer(bytes) {
        Ok(pair) => pair,
        Err(_) => fatal("create_buffer"),
    };
    // Safety: `va` is the mapping of the buffer just created, `W * H * 4`
    // bytes long.
    let mut canvas = unsafe { Canvas::new(va, W, H) };
    draw(&mut canvas);
    if display.attach_buffer(surface, buffer, bytes).is_err() {
        fatal("attach_buffer");
    }
    if display.commit(surface, Rect::new(0, 0, W, H)).is_err() {
        fatal("commit");
    }
    App {
        display,
        surface,
        events,
        canvas,
        buffer: vec![0u8; 4096],
    }
}

impl App {
    /// Repaint the whole surface from `draw` and commit it.
    fn redraw(&mut self, draw: &dyn Fn(&mut Canvas)) {
        draw(&mut self.canvas);
        let _ = self.display.commit(self.surface, Rect::new(0, 0, W, H));
    }
}

/// The drag source's state.
struct SourceState {
    /// Clipboard token offered at startup; `None` without `clipboardd`.
    token: Option<u64>,
    /// The compositor owns the pointer from `DragStart` until `DragEnded`.
    dragging: bool,
    /// Where the press landed inside the content, if any.
    press: Option<(i64, i64)>,
    /// Last pointer position the compositor reported.
    pointer: Option<(i64, i64)>,
    /// Status word drawn at the bottom.
    status: &'static str,
}

/// Draw the source's window: the draggable item and its state.
fn source_draw(canvas: &mut Canvas, state: &SourceState) {
    let clip = Rect::new(0, 0, W, H);
    canvas.fill(clip, clip, Color::rgb(12, 14, 24));
    canvas.text(8, 6, "DRAG SOURCE", Color::rgb(240, 244, 255), clip, 1);
    let item = if state.dragging {
        Color::rgb(120, 170, 240)
    } else {
        Color::rgb(200, 70, 70)
    };
    canvas.fill(ITEM, clip, item);
    canvas.fill(
        Rect::new(ITEM.x + 6, ITEM.y + 6, ITEM.w - 12, ITEM.h - 12),
        clip,
        Color::rgb(245, 245, 250),
    );
    canvas.fill(
        Rect::new(ITEM.x + 12, ITEM.y + 12, ITEM.w - 24, ITEM.h - 24),
        clip,
        item,
    );
    canvas.text(
        ITEM.x - 12,
        ITEM.y + ITEM.h + 4,
        "DRAG THIS ITEM",
        Color::rgb(230, 200, 120),
        clip,
        1,
    );
    let token = state.token.map(|token| token as i64).unwrap_or(-1);
    canvas.text(
        8,
        H - 30,
        &format!("TOKEN {token}"),
        Color::rgb(200, 210, 230),
        clip,
        1,
    );
    canvas.text(8, H - 16, state.status, Color::rgb(160, 230, 180), clip, 1);
    if let Some((x, y)) = state.pointer {
        canvas.fill(
            Rect::new(x as i32 - 2, y as i32 - 2, 5, 5),
            clip,
            Color::rgb(255, 255, 255),
        );
    }
}

/// The drag source: offer a payload, then turn a press-and-move into a drag.
fn source() -> ! {
    sys::write_str("dragdemo: drag source\n");
    let clipboard = connect_clipboard();
    let mut app = attach(connect_display(), "dragsrc", &|canvas| {
        source_draw(canvas, &SourceState::default())
    });
    let token = clipboard
        .as_ref()
        .and_then(|client| client.copy("dragdemo", &[(DEMO_MIME, PAYLOAD)]).ok());
    match token {
        Some(token) => sys::write_str(&format!("DND:SOURCE:PASS token={token}\n")),
        None => sys::write_str("DND:SKIP:no clipboardd offer\n"),
    }
    let mut state = SourceState {
        token,
        dragging: false,
        press: None,
        pointer: None,
        status: "READY",
    };
    app.redraw(&|canvas| source_draw(canvas, &state));
    loop {
        match app
            .events
            .recv_with(&mut app.buffer, Some(sys::clock() + 1))
        {
            Ok(message) => {
                if let Some(event) = display::decode_event(&message) {
                    handle_source_input(&mut app, &mut state, event);
                } else if let Some(event) = display::decode_drag_event(&message) {
                    if event.kind == DragKind::Ended {
                        state.dragging = false;
                        state.press = None;
                        if event.dropped {
                            state.status = "DROPPED";
                        } else {
                            state.status = "CANCELLED";
                            sys::write_str("DND:CANCEL:PASS\n");
                        }
                        app.redraw(&|canvas| source_draw(canvas, &state));
                    }
                }
            }
            Err(error) if is_timeout(error) => {}
            Err(Error::Errno(code)) if code == -errno::EPIPE => sys::exit(0),
            Err(_) => {}
        }
    }
}

/// Fold one input event into the source state; start the drag past threshold.
fn handle_source_input(app: &mut App, state: &mut SourceState, event: Event) {
    match event.kind {
        EventKind::PointerDown => {
            if in_item(event.a, event.b) {
                state.press = Some((event.a, event.b));
                state.status = "PRESSED";
                app.redraw(&|canvas| source_draw(canvas, state));
            }
        }
        EventKind::PointerMove => {
            state.pointer = Some((event.a, event.b));
            if !state.dragging {
                if let (Some((px, py)), Some(token)) = (state.press, state.token) {
                    if (event.a - px).abs() >= DRAG_THRESHOLD
                        || (event.b - py).abs() >= DRAG_THRESHOLD
                    {
                        match app.display.drag_start(app.surface, token, DEMO_MIME) {
                            Ok(()) => {
                                state.dragging = true;
                                state.status = "DRAGGING";
                                sys::write_str(&format!(
                                    "DND:START:PASS token={token} mime={DEMO_MIME}\n"
                                ));
                            }
                            Err(_) => {
                                state.press = None;
                                sys::write_str("DND:START:FAIL:compositor refused\n");
                            }
                        }
                    }
                }
            }
            app.redraw(&|canvas| source_draw(canvas, state));
        }
        EventKind::PointerUp => {
            state.press = None;
            if !state.dragging {
                state.status = "READY";
            }
            app.redraw(&|canvas| source_draw(canvas, state));
        }
        EventKind::KeyDown | EventKind::KeyUp => {}
    }
}

impl Default for SourceState {
    fn default() -> SourceState {
        SourceState {
            token: None,
            dragging: false,
            press: None,
            pointer: None,
            status: "READY",
        }
    }
}

/// Whether the surface-local point is inside the draggable item.
fn in_item(x: i64, y: i64) -> bool {
    x >= ITEM.x as i64
        && y >= ITEM.y as i64
        && x < (ITEM.x + ITEM.w) as i64
        && y < (ITEM.y + ITEM.h) as i64
}

/// The drop target's state.
struct TargetState {
    /// The compositor reports the pointer over this surface.
    hover: bool,
    /// Status word drawn in the drop zone.
    status: &'static str,
    /// MIME type of the last received payload.
    mime: String,
    /// Payload length of the last received token, or -1 before a drop.
    size: i64,
}

/// Draw the target's window: the drop zone and the last payload's type/len.
fn target_draw(canvas: &mut Canvas, state: &TargetState) {
    let clip = Rect::new(0, 0, W, H);
    canvas.fill(clip, clip, Color::rgb(12, 14, 24));
    canvas.text(8, 6, "DROP TARGET", Color::rgb(240, 244, 255), clip, 1);
    let zone = Rect::new(16, 34, W - 32, 86);
    let border = if state.hover {
        Color::rgb(245, 196, 84)
    } else {
        Color::rgb(92, 106, 152)
    };
    canvas.fill(zone, clip, Color::rgb(18, 24, 38));
    canvas.fill(Rect::new(zone.x, zone.y, zone.w, 2), clip, border);
    canvas.fill(
        Rect::new(zone.x, zone.y + zone.h - 2, zone.w, 2),
        clip,
        border,
    );
    canvas.fill(Rect::new(zone.x, zone.y, 2, zone.h), clip, border);
    canvas.fill(
        Rect::new(zone.x + zone.w - 2, zone.y, 2, zone.h),
        clip,
        border,
    );
    canvas.text(
        zone.x + 12,
        zone.y + 16,
        "DROP HERE",
        Color::rgb(200, 210, 230),
        clip,
        1,
    );
    canvas.text(
        zone.x + 12,
        zone.y + 34,
        state.status,
        Color::rgb(150, 160, 190),
        clip,
        1,
    );
    if state.size >= 0 {
        canvas.text(8, H - 46, &state.mime, Color::rgb(230, 200, 120), clip, 1);
        canvas.text(
            8,
            H - 30,
            &format!("{} BYTES RECEIVED", state.size),
            Color::rgb(160, 230, 180),
            clip,
            1,
        );
    }
}

/// The drop target: accept the compositor's drag events and paste the token.
fn target() -> ! {
    sys::write_str("dragdemo: drop target\n");
    let clipboard = connect_clipboard();
    let mut app = attach(connect_display(), "dragtgt", &|canvas| {
        target_draw(canvas, &TargetState::default())
    });
    let mut state = TargetState::default();
    sys::write_str("DND:TARGET:PASS\n");
    loop {
        match app
            .events
            .recv_with(&mut app.buffer, Some(sys::clock() + 1))
        {
            Ok(message) => {
                if let Some(event) = display::decode_drag_event(&message) {
                    match event.kind {
                        DragKind::Enter => {
                            state.hover = true;
                            state.status = "RELEASE TO SEND";
                            state.mime = event.mime.clone();
                            app.redraw(&|canvas| target_draw(canvas, &state));
                        }
                        DragKind::Leave => {
                            state.hover = false;
                            state.status = "WAITING FOR DRAG";
                            app.redraw(&|canvas| target_draw(canvas, &state));
                        }
                        DragKind::Drop => {
                            state.hover = false;
                            receive_drop(&mut app, &mut state, &clipboard, &event);
                        }
                        DragKind::Over | DragKind::Ended => {}
                    }
                }
            }
            Err(error) if is_timeout(error) => {}
            Err(Error::Errno(code)) if code == -errno::EPIPE => sys::exit(0),
            Err(_) => {}
        }
    }
}

/// Paste the token the compositor dropped, verify it, and probe the
/// cross-session deny path.
fn receive_drop(
    app: &mut App,
    state: &mut TargetState,
    clipboard: &Option<clipboard::Client>,
    event: &display::DragEvent,
) {
    let Some(client) = clipboard else {
        state.status = "NO CLIPBOARD";
        sys::write_str("DND:DROP:FAIL:no clipboardd\n");
        app.redraw(&|canvas| target_draw(canvas, state));
        return;
    };
    match client.paste_token(event.token, &event.mime) {
        Ok(bytes) => {
            state.mime = event.mime.clone();
            state.size = bytes.len() as i64;
            if bytes == PAYLOAD {
                state.status = "RECEIVED";
                sys::write_str(&format!(
                    "DND:DROP:PASS token={} mime={} bytes={}\n",
                    event.token,
                    event.mime,
                    bytes.len()
                ));
            } else {
                state.status = "MISMATCH";
                sys::write_str("DND:DROP:FAIL:payload mismatch\n");
            }
        }
        Err(error) => {
            state.status = "DENIED";
            sys::write_str(&format!("DND:DROP:FAIL:paste {}\n", error.message()));
        }
    }
    app.redraw(&|canvas| target_draw(canvas, state));
    denial_probe(event.token, &event.mime);
}

/// A drop must not bypass the clipboard's session scope. The probe switches
/// credentials, which cannot be undone once privilege is dropped, so it runs in
/// a short-lived child (same session, so it can resolve `clipboardd` first) and
/// the target just reaps it.
fn denial_probe(token: u64, mime: &str) {
    let command = format!("DRAGDMO.ELF probe {token} {mime}\0");
    let Some(pid) = sys::spawn(command.as_bytes()) else {
        sys::write_str("DND:DENIED:FAIL:could not start the probe\n");
        return;
    };
    let deadline = sys::clock() + PROBE_TICKS;
    while sys::clock() < deadline {
        if let Some((reaped, _)) = sys::wait(deadline) {
            if reaped == pid {
                return;
            }
        }
    }
    sys::write_str("DND:DENIED:FAIL:the probe did not exit\n");
}

/// The probe child: resolve `clipboardd`, enter another session and ask for the
/// dropped token; the service must refuse it with `-EACCES`.
fn probe(token: u64, mime: &str) -> ! {
    let Some(client) = connect_clipboard() else {
        sys::write_str("DND:DENIED:FAIL:no clipboardd\n");
        sys::exit(1);
    };
    let probe = sys::Cred::new(1000, 1000, 0, 0, PROBE_SESSION);
    if sys::cred_set(None, &probe).is_err() {
        sys::write_str("DND:DENIED:FAIL:could not enter the probe session\n");
        sys::exit(1);
    }
    match client.paste_token(token, mime) {
        Err(Error::Errno(code)) if code == -errno::EACCES => {
            sys::write_str("DND:DENIED:PASS\n");
            sys::exit(0)
        }
        Ok(_) => sys::write_str("DND:DENIED:FAIL:cross-session paste was allowed\n"),
        Err(error) => sys::write_str(&format!("DND:DENIED:FAIL:{}\n", error.message())),
    }
    sys::exit(1)
}

impl Default for TargetState {
    fn default() -> TargetState {
        TargetState {
            hover: false,
            status: "WAITING FOR DRAG",
            mime: String::new(),
            size: -1,
        }
    }
}

/// Connect to the compositor, retrying while it binds and registers.
fn connect_display() -> Client {
    for _ in 0..CONNECT_ATTEMPTS {
        if let Ok(client) = Client::connect() {
            return client;
        }
        park_tick();
    }
    fatal("no compositor")
}

/// Resolve `clipboardd`, retrying while the supervisor starts it. `None` when
/// it never appears (the plain `LAZYOS_XUID=1` demo boots without services).
fn connect_clipboard() -> Option<clipboard::Client> {
    for _ in 0..CONNECT_ATTEMPTS {
        if let Ok(client) = clipboard::Client::connect() {
            return Some(client);
        }
        park_tick();
    }
    None
}

/// Whether an error is the `recv` deadline firing.
fn is_timeout(error: Error) -> bool {
    matches!(error, Error::Errno(code) if code == -errno::ETIMEDOUT)
}

/// Sleep one PIT tick: userspace has no sleep syscall, and parking on the
/// child-exit queue returns at the deadline when there is nothing to reap.
fn park_tick() {
    let _ = sys::wait(sys::clock() + 1);
}

/// Report a fatal setup failure on serial, then exit non-zero.
fn fatal(what: &str) -> ! {
    sys::write_str("dragdemo: fatal: ");
    sys::write_str(what);
    sys::write_str("\n");
    sys::exit(1)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::write_str("dragdemo: panic\n");
    sys::exit(1)
}
