//! Resident xui apps (docs/tray-plan.md section 5.1): an app that keeps
//! running with no window, lives in the tray, opens a window on demand and
//! closes it back to the tray.
//!
//! `xui_core::app::run_app` needs a window: it opens one, runs the backend
//! loop until that window closes or the app quits, and returns. So the
//! windowless part is this module's own loop ([`Resident::idle`]), parked on
//! the tray item's channel and the lifecycle channel, and a window is one
//! [`Resident::window`] call that returns when the window closes (close to
//! tray) or the app asked to quit. The app owns the choices (section 5.1):
//! what a `Reopen`, an `Activate` or a `Quit` means is its code.
//!
//! [`Lifecycle`] is the app's line to `init` (`os.lazy.init.app.v1`): `Watch`
//! hands `init` a channel, and `init` sends `Reopen(args)` when the app is
//! launched again and `Quit(grace_ms)` when someone stops it (the grace is a
//! hard 3 s counted from the `Stop`; `init` kills the app after it).

use std::cell::RefCell;
use std::rc::Rc;

use libmessenger::Parcel;
use messenger_generated::os_lazy_init_app_events_v1 as events;
use messenger_generated::os_lazy_init_app_v1 as app_wire;
use xui_core::app::{App, Ui};
use xui_core::backend::Backend;

use crate::backend::LazyOSBackend;
use crate::platform::messenger::Service;
use crate::server::ERROR_FIELD;
use crate::sys::{self, errno};
use crate::tray::TrayIcon;

/// `init`'s lifecycle service.
const INIT_APP: &str = "os.lazy.init.app";
/// Ticks one `Watch` may take.
const WATCH_TICKS: u64 = 100;
/// The longest a windowless app parks before it looks at its topics again
/// (the tray's generation): half a second.
const IDLE_MILLIS: u64 = 500;
/// Receive buffer for one lifecycle event.
const EVENT_BYTES: usize = 2048;

/// Something that woke a resident app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wake {
    /// The tray item was clicked, scrolled or its menu used.
    Tray(trayclient::Event),
    /// The app was launched again: show yourself, open `args` if any.
    Reopen(String),
    /// Save and exit within `grace_ms`.
    Quit(u32),
}

/// The app's lifecycle channel from `init`.
pub struct Lifecycle {
    channel: u64,
    buf: Vec<u8>,
}

impl Lifecycle {
    /// `Watch`: ask `init` for this instance's lifecycle events. `Reopen`s
    /// queued before it arrive at once. Fails with `ESRCH` when `init` did
    /// not launch this task.
    pub fn watch() -> Result<Lifecycle, i64> {
        let (sent, kept) = sys::msg_create_pair()?;
        let result = Service::try_connect(INIT_APP)
            .ok_or(-errno::ENOENT)
            .and_then(|init| {
                init.call_moving_within(
                    app_wire::INTERFACE_ID,
                    app_wire::METHOD_WATCH,
                    ERROR_FIELD,
                    Vec::new(),
                    vec![sent],
                    WATCH_TICKS,
                )
            });
        match result {
            Ok(_) => Ok(Lifecycle {
                channel: kept,
                buf: vec![0; EVENT_BYTES],
            }),
            Err(code) => {
                let _ = sys::msg_close(sent);
                let _ = sys::msg_close(kept);
                Err(code)
            }
        }
    }

    /// The events queued since the last poll; never parks.
    pub fn poll(&mut self) -> Vec<Wake> {
        let mut found = Vec::new();
        while matches!(sys::msg_queued(self.channel), Ok(count) if count > 0) {
            let Ok(result) = sys::msg_recv(self.channel, &mut self.buf, sys::EXPIRED_DEADLINE)
            else {
                break;
            };
            let Some(bytes) = self.buf.get(..result.bytes as usize) else {
                continue;
            };
            if let Some(wake) = Parcel::decode(bytes).ok().and_then(|parcel| decode(&parcel)) {
                found.push(wake);
            }
        }
        found
    }
}

/// One lifecycle event, or `None` for anything else.
fn decode(parcel: &Parcel) -> Option<Wake> {
    if parcel.header.interface_id != events::INTERFACE_ID {
        return None;
    }
    match parcel.header.method {
        events::METHOD_REOPEN => events::decode_reopen_args(&parcel.body)
            .ok()
            .map(|args| Wake::Reopen(args.args)),
        events::METHOD_QUIT => events::decode_quit_args(&parcel.body)
            .ok()
            .map(|args| Wake::Quit(args.grace_ms)),
        _ => None,
    }
}

/// A resident app's runtime: its backend, tray item and lifecycle line.
pub struct Resident {
    pub backend: Rc<LazyOSBackend>,
    pub tray: RefCell<TrayIcon>,
    life: RefCell<Option<Lifecycle>>,
}

impl Resident {
    /// Connect to the compositor and `init` (a missing lifecycle line is
    /// reported as `<marker>:WATCH:FAIL err=<n>` and leaves the app without
    /// `Reopen`/`Quit`, killed at the end of a `Stop`'s grace).
    pub fn connect(marker: &str) -> Result<Rc<Resident>, i64> {
        let backend = Rc::new(LazyOSBackend::connect()?);
        let life = match Lifecycle::watch() {
            Ok(life) => {
                println!("{marker}:WATCH:PASS");
                Some(life)
            }
            Err(code) => {
                println!("{marker}:WATCH:FAIL err={}", -code);
                None
            }
        };
        Ok(Rc::new(Resident {
            backend,
            tray: RefCell::new(TrayIcon::new()),
            life: RefCell::new(life),
        }))
    }

    /// Ask for the lifecycle line again (after it was refused at start-up,
    /// or for an app that watches late).
    pub fn watch(&self) -> Result<(), i64> {
        *self.life.borrow_mut() = Some(Lifecycle::watch()?);
        Ok(())
    }

    /// What woke the app since the last look; never parks. Call it from a
    /// window's timer while a window is open.
    pub fn poll(&self) -> Vec<Wake> {
        let mut found: Vec<Wake> = match self.life.borrow_mut().as_mut() {
            Some(life) => life.poll(),
            None => Vec::new(),
        };
        found.extend(self.tray.borrow_mut().poll().into_iter().map(Wake::Tray));
        found
    }

    /// The windowless wait: park on the tray and lifecycle channels for at
    /// most `timeout_ms` (capped at [`IDLE_MILLIS`], so the tray generation
    /// is followed), then return what woke the app.
    pub fn idle(&self, timeout_ms: u64) -> Vec<Wake> {
        let mut handles = Vec::with_capacity(2);
        if let Some(life) = self.life.borrow().as_ref() {
            handles.push(life.channel);
        }
        if let Some(channel) = self.tray.borrow().channel() {
            handles.push(channel);
        }
        let ms = timeout_ms.clamp(1, IDLE_MILLIS);
        if handles.is_empty() {
            sys::sleep_millis(ms);
        } else {
            let deadline = sys::clock_ticks().saturating_add(ms.div_ceil(10));
            let _ = sys::msg_wait_any(&handles, deadline);
        }
        self.poll()
    }

    /// Open a window titled `title` (`size` in design pixels) built by
    /// `make`, and run it until it closes or the app quits; the app is then
    /// windowless again. `make` gets the window's `Ui`.
    pub fn window<A, F>(&self, title: &str, size: (i32, i32), make: F) -> Result<(), String>
    where
        A: App,
        F: FnOnce(&mut Ui<A::Msg>) -> xui_core::backend::Result<A>,
    {
        self.backend.rearm();
        let (width, height) = self.backend.window_size(size);
        xui_core::app(title)
            .size(width, height)
            .backend(Rc::clone(&self.backend) as Rc<dyn Backend>)
            .run(make)
            .map_err(|error| error.to_string())
    }
}
