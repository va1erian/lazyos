//! webd's built-in client (`Launch os.lazy.webd` with the path `/selftest`: `init` sends the running
//! instance `Reopen("/selftest")`, since webd is resident and single).
//!
//! It exercises the service through the **real** road, as an app would: a thread of the process
//! resolves `app.os.lazy.webd.light` in its own handle table, moves an event channel in with
//! `CreateView`, shares pixel buffers with `AttachSlot` and `LoadHtml`, reads the events and the pixels,
//! then ends the web process under the view with `SIGKILL` and expects `ViewLost`, and makes a new view.
//! The only process it ever kills is the one that serves its own view, and only when it can tell which
//! that is ([`own_web_process`]); the views it made are destroyed on every way out ([`Client`]'s `Drop`).
//! Each step is a `WEBD:SELFTEST:*` line; the last is `WEBD:SELFTEST:PASS` or `WEBD:SELFTEST:FAIL:<why>`.
//!
//! It proves the Messenger binding, the kernel's shared buffers and the engine end to end in an
//! image with no other client. It cannot prove the network is untouched (a harness with a listener
//! does that: `webd-abi-test network`, `tools/webd/run.py`) and it runs as webd's own label, so it does
//! not prove the manifest grants a *client* what it needs: `webddemo` does.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Instant;

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
/// The web process an earlier run of the test proved was its own (0: none yet): the engine's "newest
/// web process" is the same number before and after a view that reused it, which says nothing then.
static OWN_WEB: AtomicI32 = AtomicI32::new(0);

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
    /// The end of the event channel this thread keeps, while it has one.
    events: Option<u64>,
    /// The view this thread has made and not destroyed.
    live: Option<u64>,
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
            events: None,
            live: None,
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
        self.events = Some(kept);
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
        // From here on the view is this thread's to end, whatever fails next.
        self.live = Some(view);
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
        if self.live == Some(view) {
            self.live = None;
        }
        self.close_local();
    }

    /// Close this thread's side of the view: the events and the pixel buffers.
    fn close_local(&mut self) {
        if let Some(events) = self.events.take() {
            let _ = sys::msg_close(events);
        }
        for (handle, _) in self.slots.drain(..) {
            let _ = sys::buffer_close(handle);
        }
    }

    /// The next event on the channel, or `None` after `WAIT_TICKS`.
    fn next_event(&self) -> Option<Event> {
        let mut buf = vec![0u8; 4096];
        let deadline = sys::clock_ticks() + WAIT_TICKS;
        let result = sys::msg_recv(self.events?, &mut buf, deadline).ok()?;
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

impl Drop for Client {
    /// Every way out of the test ends the view it made (a failed step must not leave one behind in
    /// webd, holding a web process and its share of the quota), and gives the resolved endpoint back.
    fn drop(&mut self) {
        match self.live.take() {
            Some(view) => self.destroy(view),
            None => self.close_local(),
        }
        let _ = sys::msg_release(self.endpoint);
    }
}

impl Client {
    /// How long `Refresh` takes to come back as a frame (the hop out, the engine's paint and the hop
    /// back), `count` times: milliseconds, sorted.
    fn refresh_times(&self, view: u64, count: usize) -> Result<Vec<u128>, String> {
        let mut times = Vec::new();
        for _ in 0..count {
            let body = wire::encode_refresh_args(&wire::RefreshArgs { view })
                .map_err(|_| "encode Refresh".to_string())?;
            let began = Instant::now();
            sys::msg_send(self.endpoint, &oneway(wire::METHOD_REFRESH, body))
                .map_err(|code| format!("Refresh: err={}", -code))?;
            loop {
                match self.next_event() {
                    Some(Event::Frame { slot, .. }) => {
                        times.push(began.elapsed().as_millis());
                        self.release(view, slot);
                        break;
                    }
                    Some(Event::Lost { reason }) => return Err(format!("view lost: {reason}")),
                    Some(Event::Other) => {}
                    None => return Err("no frame after Refresh".into()),
                }
            }
        }
        times.sort_unstable();
        Ok(times)
    }
}

/// The newest web process the engine knows of.
fn newest_web() -> i32 {
    FfiEngine.children().0
}

/// The id of the web process that serves this test's view, or why that cannot be told.
///
/// The engine reports only the *newest* web process of any client (`webd_engine_children`), and a
/// `SIGKILL` of another client's page is not a test of anything. The test's view is the only one in
/// the group of webd's own label, so it has a process of its own, and that process is the newest one
/// right after it was announced. `before` is the newest before the view was made, `first` and `second`
/// the newest at two moments after its first frame, `known` the process an earlier run proved to be its
/// own. The answer is `first` when it is a process the view brought (it is not `before`, or it is
/// `known`) and the two samples agree; otherwise there is no answer, and nothing is killed.
fn own_web_process(before: i32, first: i32, second: i32, known: i32) -> Result<i32, String> {
    if first <= 0 {
        return Err("no web process id".into());
    }
    if first != second {
        return Err(format!(
            "the newest web process changed from {first} to {second} (another client is starting a page): cannot tell which one is ours"
        ));
    }
    if first == before && first != known {
        return Err(format!(
            "the newest web process, {first}, is the one that was there before the view: cannot tell it from another client's"
        ));
    }
    Ok(first)
}

/// The test's own web process after its view painted: two looks, a moment apart.
fn find_own_web(before: i32) -> Result<i32, String> {
    let first = newest_web();
    std::thread::sleep(std::time::Duration::from_millis(250));
    own_web_process(before, first, newest_web(), OWN_WEB.load(Ordering::Acquire))
}

fn step(text: &str) {
    trace::line!(Channel::Webd, "SELFTEST:STEP {text}");
}

fn run(hold: bool) -> Result<String, String> {
    let mut client = Client::new()?;
    let before = newest_web();
    // 1. A light view paints the colour of the page it was given.
    let view = client.create_view()?;
    step("created");
    let began = Instant::now();
    client
        .load_html(view, &page(GREEN))
        .map_err(|code| format!("LoadHtml: err={}", -code))?;
    let frames = client.wait_color(view, GREEN)?;
    step(&format!(
        "green frame after {frames} frames, {} ms from LoadHtml",
        began.elapsed().as_millis()
    ));
    // 2. A second load replaces the page and the slot is reused.
    let began = Instant::now();
    client
        .load_html(view, &page(RED))
        .map_err(|code| format!("LoadHtml: err={}", -code))?;
    client.wait_color(view, RED)?;
    step(&format!(
        "red frame, {} ms from LoadHtml",
        began.elapsed().as_millis()
    ));
    let times = client.refresh_times(view, 20)?;
    step(&format!(
        "refresh to frame, 20 times: min {} median {} max {} ms",
        times[0],
        times[times.len() / 2],
        times[times.len() - 1]
    ));
    // 3. The web process dies: the client is told, the view answers ENODEV, a new one works.
    let web = find_own_web(before)?;
    OWN_WEB.store(web, Ordering::Release);
    step(&format!("web process {web} serves this view"));
    // SAFETY: a plain `kill` of the web process that `find_own_web` told apart as the one serving
    // this test's own view, a child of this service.
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
    // The process of the new view is this test's own too: remember it for the next run.
    OWN_WEB.store(find_own_web(web).unwrap_or(0), Ordering::Release);
    if hold {
        trace::line!(Channel::Webd, "SELFTEST:HOLD seconds={HOLD_SECS}");
        std::thread::sleep(std::time::Duration::from_secs(HOLD_SECS));
    }
    client.destroy(view);
    Ok("frame colours right, ViewLost on a killed web process, recovered".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_process_that_stays_the_newest_is_ours() {
        assert_eq!(own_web_process(0, 41, 41, 0), Ok(41));
        assert_eq!(own_web_process(40, 41, 41, 0), Ok(41));
    }

    #[test]
    fn a_process_that_was_already_the_newest_is_ours_only_if_an_earlier_run_proved_it() {
        assert!(own_web_process(41, 41, 41, 0).is_err());
        assert!(own_web_process(41, 41, 41, 40).is_err());
        assert_eq!(own_web_process(41, 41, 41, 41), Ok(41));
    }

    #[test]
    fn nothing_is_killed_when_the_newest_changes_or_is_none() {
        assert!(own_web_process(0, 0, 0, 0).is_err());
        assert!(own_web_process(0, -1, -1, 0).is_err());
        assert!(
            own_web_process(0, 41, 42, 0).is_err(),
            "another client started a page"
        );
        assert!(
            own_web_process(40, 41, 0, 0).is_err(),
            "it ended while we looked"
        );
    }
}
