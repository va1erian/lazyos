//! A recording [`Engine`] for the host tests (slice 2's tests use it too): every call is appended
//! to a log, `children` answers what the test set, and nothing else happens. The tests play the
//! engine's side by calling the [`crate::events::EventQueue`] the service drains.

use std::sync::{Arc, Mutex};

use crate::engine::{Engine, EngineConfig};

/// One call the service made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    ViewCreate(u64, EngineConfig),
    ViewDestroy(u64),
    LoadHtml {
        view: u64,
        html: Vec<u8>,
        base_url: String,
    },
    Resize(u64, i32, i32),
    SetVisible(u64, bool),
    Refresh(u64),
    Mouse {
        view: u64,
        kind: i32,
        button: i32,
        x: i32,
        y: i32,
        mods: u32,
    },
    Wheel {
        view: u64,
        x: i32,
        y: i32,
        ticks_x: i32,
        ticks_y: i32,
        mods: u32,
    },
    Key {
        view: u64,
        press: bool,
        keysym: u32,
        text: String,
        mods: u32,
    },
    Focus(u64, bool),
    Quit,
    // ---- the full views' calls (the trait's defaults do nothing; a test that judges what the
    // service told the engine about a full view needs them) ----
    Load(u64, String),
    Reload(u64),
    Stop(u64),
    Back(u64),
    Forward(u64),
    Download(u64, String),
    DownloadCancel(u64, u64),
    DialogReply {
        view: u64,
        dialog: u64,
        accepted: bool,
        value: String,
    },
    NavigationAnswer {
        view: u64,
        request: u64,
        allow: bool,
    },
    NotifyShown(u64),
    NotifyClicked(u64),
    NotifyClosed(u64),
    NotifyPermission {
        view: u64,
        origin: String,
        answer: i32,
    },
    BlockingLoad(u64, String),
    BlockingCompile {
        view: u64,
        list: String,
        json: Vec<u8>,
    },
    BlockingForget(u64, String),
    // ---- P4 ----
    Recover(u64, bool),
    NotifyForget(u64, String),
    RecoverGroup(u64),
    SetUserAgent(u64, String),
}

#[derive(Default)]
struct State {
    calls: Vec<Call>,
    children: (i32, i32),
}

/// The recording double. Clones share one log, so a test keeps a handle after giving one to the service.
#[derive(Clone, Default)]
pub struct RecordingEngine {
    state: Arc<Mutex<State>>,
}

impl RecordingEngine {
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&self, call: Call) {
        self.lock().calls.push(call);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Every call so far.
    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// Take the log, leaving it empty.
    pub fn take(&self) -> Vec<Call> {
        std::mem::take(&mut self.lock().calls)
    }

    /// What `children` answers.
    pub fn set_children(&self, web: i32, network: i32) {
        self.lock().children = (web, network);
    }

    /// How many times `view` was created and how many times destroyed.
    pub fn creates_and_destroys(&self, view: u64) -> (usize, usize) {
        let calls = self.lock();
        let count = |wanted: fn(&Call, u64) -> bool| {
            calls.calls.iter().filter(|call| wanted(call, view)).count()
        };
        (
            count(|call, view| matches!(call, Call::ViewCreate(id, _) if *id == view)),
            count(|call, view| matches!(call, Call::ViewDestroy(id) if *id == view)),
        )
    }
}

impl Engine for RecordingEngine {
    fn view_create(&self, view: u64, config: &EngineConfig) {
        self.push(Call::ViewCreate(view, config.clone()));
    }

    fn view_destroy(&self, view: u64) {
        self.push(Call::ViewDestroy(view));
    }

    fn load_html(&self, view: u64, html: &[u8], base_url: &str) {
        self.push(Call::LoadHtml {
            view,
            html: html.to_vec(),
            base_url: base_url.to_owned(),
        });
    }

    fn resize(&self, view: u64, width: i32, height: i32) {
        self.push(Call::Resize(view, width, height));
    }

    fn set_visible(&self, view: u64, visible: bool) {
        self.push(Call::SetVisible(view, visible));
    }

    fn refresh(&self, view: u64) {
        self.push(Call::Refresh(view));
    }

    fn mouse(&self, view: u64, kind: i32, button: i32, x: i32, y: i32, mods: u32) {
        self.push(Call::Mouse {
            view,
            kind,
            button,
            x,
            y,
            mods,
        });
    }

    fn wheel(&self, view: u64, x: i32, y: i32, ticks_x: i32, ticks_y: i32, mods: u32) {
        self.push(Call::Wheel {
            view,
            x,
            y,
            ticks_x,
            ticks_y,
            mods,
        });
    }

    fn key(&self, view: u64, press: bool, keysym: u32, text: &str, mods: u32) {
        self.push(Call::Key {
            view,
            press,
            keysym,
            text: text.to_owned(),
            mods,
        });
    }

    fn focus(&self, view: u64, focused: bool) {
        self.push(Call::Focus(view, focused));
    }

    fn quit(&self) {
        self.push(Call::Quit);
    }

    fn children(&self) -> (i32, i32) {
        self.lock().children
    }

    fn load(&self, view: u64, url: &str) {
        self.push(Call::Load(view, url.to_owned()));
    }

    fn reload(&self, view: u64) {
        self.push(Call::Reload(view));
    }

    fn stop(&self, view: u64) {
        self.push(Call::Stop(view));
    }

    fn back(&self, view: u64) {
        self.push(Call::Back(view));
    }

    fn forward(&self, view: u64) {
        self.push(Call::Forward(view));
    }

    fn download(&self, view: u64, url: &str) {
        self.push(Call::Download(view, url.to_owned()));
    }

    fn download_cancel(&self, view: u64, download: u64) {
        self.push(Call::DownloadCancel(view, download));
    }

    fn dialog_reply(&self, view: u64, dialog: u64, accepted: bool, value: &str) {
        self.push(Call::DialogReply {
            view,
            dialog,
            accepted,
            value: value.to_owned(),
        });
    }

    fn navigation_answer(&self, view: u64, request: u64, allow: bool) {
        self.push(Call::NavigationAnswer {
            view,
            request,
            allow,
        });
    }

    fn notify_shown(&self, id: u64) {
        self.push(Call::NotifyShown(id));
    }

    fn notify_clicked(&self, id: u64) {
        self.push(Call::NotifyClicked(id));
    }

    fn notify_closed(&self, id: u64) {
        self.push(Call::NotifyClosed(id));
    }

    fn notify_permission(&self, view: u64, origin: &str, answer: i32) {
        self.push(Call::NotifyPermission {
            view,
            origin: origin.to_owned(),
            answer,
        });
    }

    fn blocking_load(&self, view: u64, list: &str) {
        self.push(Call::BlockingLoad(view, list.to_owned()));
    }

    fn blocking_compile(&self, view: u64, list: &str, json: &[u8]) {
        self.push(Call::BlockingCompile {
            view,
            list: list.to_owned(),
            json: json.to_vec(),
        });
    }

    fn blocking_forget(&self, view: u64, list: &str) {
        self.push(Call::BlockingForget(view, list.to_owned()));
    }

    fn recover(&self, view: u64, reload: bool) {
        self.push(Call::Recover(view, reload));
    }

    fn recover_group(&self, view: u64) {
        self.push(Call::RecoverGroup(view));
    }

    fn set_user_agent(&self, view: u64, agent: &str) {
        self.push(Call::SetUserAgent(view, agent.to_owned()));
    }

    fn notify_forget(&self, view: u64, origin: &str) {
        self.push(Call::NotifyForget(view, origin.to_owned()));
    }
}
