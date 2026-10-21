//! webd's built-in client (`Launch os.lazy.webd` with the path `/selftest`: `init` sends the running
//! instance `Reopen("/selftest")`, since webd is resident and single).
//!
//! It exercises the service through the **real** road, as an app would: a thread of the process
//! resolves `app.os.lazy.webd.light` in its own handle table, moves an event channel in with
//! `CreateView`, shares pixel buffers with `AttachSlot` and `LoadHtml`, reads the events and the pixels,
//! then ends the web process under the view with `SIGKILL` and expects `ViewLost`, and makes a new view.
//! Each step is a `WEBD:SELFTEST:*` line; the last is `WEBD:SELFTEST:PASS` or `WEBD:SELFTEST:FAIL:<why>`.
//!
//! It proves the Messenger binding, the kernel's shared buffers and the engine end to end in an
//! image with no other client. It cannot prove the network is untouched (a harness with a listener
//! does that: `webd-abi-test network`, `tools/webd/run.py`) and it runs as webd's own label, so it does
//! not prove the manifest grants a *client* what it needs: `webddemo` does.

use std::sync::atomic::{AtomicBool, Ordering};

use libmessenger::{flags, Buffer, Header, Parcel, VERSION};
use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_webd_light_v1 as wire;
use xui_app::platform::messenger::Service;
use xui_app::sys::{self, errno};
use xui_app::trace::{self, Channel};
use xui_webd::engine::Engine;
use xui_webd::ffi::FfiEngine;

use crate::serve::NAME;

/// The `Reopen` argument that starts the test (`init` takes one absolute path as `args`).
pub const ARG: &str = "/selftest";
/// The same test, keeping its last view alive for [`HOLD_SECS`] before it ends it: a view to measure.
pub const HOLD_ARG: &str = "/selftest/hold";
const HOLD_SECS: u64 = 40;

static RUNNING: AtomicBool = AtomicBool::new(false);

const WIDTH: u32 = 160;
const HEIGHT: u32 = 90;
/// The pages' background colours as the RGBA pixels the engine must paint.
const GREEN: [u8; 4] = [0x2e, 0xa0, 0x43, 0xff];
const RED: [u8; 4] = [0xc8, 0x32, 0x1e, 0xff];
/// Ticks (10 ms) a step may wait.
const WAIT_TICKS: u64 = 3000;

/// Run the test on a thread of its own, unless one is running; `hold` keeps the last view for a while.
pub fn start(hold: bool) {
    if RUNNING.swap(true, Ordering::AcqRel) {
        trace::line!(Channel::Webd, "SELFTEST:BUSY");
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("webd-selftest".into())
        .stack_size(1 << 20)
        .spawn(move || {
            match run(hold) {
                Ok(summary) => trace::pass!(Channel::Webd, "SELFTEST", "{summary}"),
                Err(why) => trace::fail!(Channel::Webd, "SELFTEST", "{why}"),
            }
            RUNNING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        RUNNING.store(false, Ordering::Release);
        trace::fail!(Channel::Webd, "SELFTEST", "cannot start the thread");
    }
}

/// A page of one colour.
fn page(color: [u8; 4]) -> Vec<u8> {
    format!(
        "<!doctype html><title>selftest</title><body style=\"margin:0;background:#{:02x}{:02x}{:02x}\">",
        color[0], color[1], color[2]
    )
    .into_bytes()
}

/// This thread's side of the conversation.
struct Client {
    service: Service,
    /// A second handle on the service, for the oneway messages `Service` has no call for.
    endpoint: u64,
    /// The end of the event channel this thread keeps.
    events: u64,
    /// The pixel buffers this thread attached: handle and the address it mapped.
    slots: Vec<(u64, u64)>,
}

fn oneway(method: u32, body: Vec<u8>) -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: wire::INTERFACE_ID,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        objects: Vec::new(),
    }
}

/// One event from the service.
enum Event {
    Frame { slot: u32, width: u32, height: u32 },
    Lost { reason: u32 },
    Other,
}

impl Client {
    fn new() -> Result<Client, String> {
        let service = Service::connect(NAME).map_err(|code| format!("connect: err={}", -code))?;
        let endpoint = sys::msg_resolve(NAME).map_err(|code| format!("resolve: err={}", -code))?;
        Ok(Client {
            service,
            endpoint,
            events: 0,
            slots: Vec::new(),
        })
    }

    fn call(
        &self,
        method: u32,
        (body, objects): (Vec<u8>, Vec<libmessenger::Object>),
    ) -> Result<Parcel, i64> {
        self.service.call_moving_within(
            wire::INTERFACE_ID,
            method,
            ERROR_FIELD,
            body,
            objects,
            1000,
        )
    }

    /// `CreateView`, with two slots attached.
    fn create_view(&mut self) -> Result<u64, String> {
        let (sent, kept) = sys::msg_create_pair().map_err(|code| format!("pair: err={}", -code))?;
        self.events = kept;
        let args = wire::encode_create_view_args(&wire::CreateViewArgs {
            config: wire::LightConfig::default(),
            width: WIDTH,
            height: HEIGHT,
            events: sent,
        })
        .map_err(|_| "encode CreateView".to_string())?;
        let reply = self
            .call(wire::METHOD_CREATEVIEW, args)
            .map_err(|code| format!("CreateView: err={}", -code))?;
        let view = wire::decode_create_view_reply(&reply.body)
            .map_err(|_| "CreateView reply".to_string())?
            .view;
        self.slots.clear();
        for slot in 0..2u32 {
            let bytes = u64::from(WIDTH) * u64::from(HEIGHT) * 4;
            let (handle, address, _size) =
                sys::buffer_create(bytes).map_err(|code| format!("buffer: err={}", -code))?;
            self.slots.push((handle, address));
            let args = wire::encode_attach_slot_args(&wire::AttachSlotArgs {
                view,
                slot,
                pixels: Buffer::whole(handle, bytes),
            })
            .map_err(|_| "encode AttachSlot".to_string())?;
            self.call(wire::METHOD_ATTACHSLOT, args)
                .map_err(|code| format!("AttachSlot {slot}: err={}", -code))?;
        }
        Ok(view)
    }

    fn load_html(&self, view: u64, html: &[u8]) -> Result<(), i64> {
        let (handle, address, _size) = sys::buffer_create(html.len() as u64)?;
        // SAFETY: `buffer_create` mapped at least `html.len()` bytes at `address` for this task.
        unsafe { std::ptr::copy_nonoverlapping(html.as_ptr(), address as *mut u8, html.len()) };
        let args = wire::encode_load_html_args(&wire::LoadHtmlArgs {
            view,
            html: Buffer::whole(handle, html.len() as u64),
            base_url: "about:blank".into(),
        })
        .map_err(|_| -errno::EINVAL)?;
        let result = self.call(wire::METHOD_LOADHTML, args).map(drop);
        // The service copied the bytes before it replied.
        let _ = sys::buffer_close(handle);
        result
    }

    fn release(&self, view: u64, slot: u32) {
        if let Ok(body) = wire::encode_release_slot_args(&wire::ReleaseSlotArgs { view, slot }) {
            let _ = sys::msg_send(self.endpoint, &oneway(wire::METHOD_RELEASESLOT, body));
        }
    }

    fn destroy(&mut self, view: u64) {
        if let Ok(body) = wire::encode_destroy_view_args(&wire::DestroyViewArgs { view }) {
            let _ = self.call(wire::METHOD_DESTROYVIEW, (body, Vec::new()));
        }
        let _ = sys::msg_close(self.events);
        for (handle, _) in self.slots.drain(..) {
            let _ = sys::buffer_close(handle);
        }
    }

    /// The next event on the channel, or `None` after `WAIT_TICKS`.
    fn next_event(&self) -> Option<Event> {
        let mut buf = vec![0u8; 4096];
        let deadline = sys::clock_ticks() + WAIT_TICKS;
        let result = sys::msg_recv(self.events, &mut buf, deadline).ok()?;
        let parcel = Parcel::decode(buf.get(..result.bytes as usize)?).ok()?;
        Some(match parcel.header.method {
            wire::METHOD_FRAME => {
                let frame = wire::decode_frame_args(&parcel.body).ok()?;
                Event::Frame {
                    slot: frame.slot,
                    width: frame.width,
                    height: frame.height,
                }
            }
            wire::METHOD_VIEWLOST => Event::Lost {
                reason: wire::decode_view_lost_args(&parcel.body).ok()?.reason,
            },
            _ => Event::Other,
        })
    }

    /// The pixel at the middle of the slot's picture.
    fn middle(&self, slot: u32) -> Option<[u8; 4]> {
        let (_, address) = *self.slots.get(slot as usize)?;
        let at = (u64::from(HEIGHT / 2) * u64::from(WIDTH) + u64::from(WIDTH / 2)) * 4;
        let mut pixel = [0u8; 4];
        // SAFETY: the slot was created with `WIDTH * HEIGHT * 4` bytes, mapped in this task, and
        // `at + 4` lies inside it.
        unsafe {
            std::ptr::copy_nonoverlapping((address + at) as *const u8, pixel.as_mut_ptr(), 4)
        };
        Some(pixel)
    }

    /// Frames until one whose middle pixel is `color` (an earlier page's frame is released and
    /// skipped); the number of frames seen.
    fn wait_color(&self, view: u64, color: [u8; 4]) -> Result<u32, String> {
        let mut seen = 0;
        loop {
            match self.next_event() {
                Some(Event::Frame {
                    slot,
                    width,
                    height,
                }) => {
                    seen += 1;
                    if (width, height) != (WIDTH, HEIGHT) {
                        return Err(format!("frame of {width}x{height}"));
                    }
                    let pixel = self.middle(slot).ok_or("no such slot")?;
                    self.release(view, slot);
                    if pixel == color {
                        return Ok(seen);
                    }
                    if seen > 40 {
                        return Err(format!("no frame of {color:02x?}; last {pixel:02x?}"));
                    }
                }
                Some(Event::Lost { reason }) => return Err(format!("view lost: reason={reason}")),
                Some(Event::Other) => {}
                None => return Err(format!("no frame of {color:02x?} within the wait")),
            }
        }
    }
}

fn step(text: &str) {
    trace::line!(Channel::Webd, "SELFTEST:STEP {text}");
}

fn run(hold: bool) -> Result<String, String> {
    let mut client = Client::new()?;
    // 1. A light view paints the colour of the page it was given.
    let view = client.create_view()?;
    step("created");
    client
        .load_html(view, &page(GREEN))
        .map_err(|code| format!("LoadHtml: err={}", -code))?;
    let frames = client.wait_color(view, GREEN)?;
    step(&format!("green frame after {frames}"));
    // 2. A second load replaces the page and the slot is reused.
    client
        .load_html(view, &page(RED))
        .map_err(|code| format!("LoadHtml: err={}", -code))?;
    client.wait_color(view, RED)?;
    step("red frame");
    // 3. The web process dies: the client is told, the view answers ENODEV, a new one works.
    let (web, _network) = FfiEngine.children();
    if web <= 0 {
        return Err("no web process id".into());
    }
    // SAFETY: a plain `kill` of the web process this service started.
    unsafe { libc::kill(web, libc::SIGKILL) };
    loop {
        match client.next_event() {
            Some(Event::Lost { reason }) => {
                step(&format!("view lost: reason={reason}"));
                if reason != wire::LOST_REASON_CRASHED {
                    return Err(format!("lost with reason {reason}, wanted crashed"));
                }
                break;
            }
            Some(_) => {}
            None => return Err("no ViewLost after the web process was killed".into()),
        }
    }
    match client.load_html(view, &page(GREEN)) {
        Err(code) if code == -errno::ENODEV => step("lost view answers ENODEV"),
        other => return Err(format!("LoadHtml on a lost view: {other:?}")),
    }
    client.destroy(view);
    let view = client.create_view()?;
    client
        .load_html(view, &page(GREEN))
        .map_err(|code| format!("LoadHtml: err={}", -code))?;
    client.wait_color(view, GREEN)?;
    step("recovered");
    if hold {
        trace::line!(Channel::Webd, "SELFTEST:HOLD seconds={HOLD_SECS}");
        std::thread::sleep(std::time::Duration::from_secs(HOLD_SECS));
    }
    client.destroy(view);
    Ok("frame colours right, ViewLost on a killed web process, recovered".into())
}
