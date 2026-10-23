//! The Rust shape of the C ABI between webd's two halves (`webkit/port/webd/webd_abi.h`): what the
//! service asks of the engine ([`Engine`], the `webd_engine_*` functions) and what the engine tells
//! the service ([`EngineEvents`], the `webd_ui_*` callbacks).
//!
//! Threads follow the header. Every [`Engine`] method may be called from any thread, only queues work
//! for WebKit's run loop and returns. Every [`EngineEvents`] method is called on WebKit's main thread
//! (`say` from any thread) and must return without waiting for the service: [`crate::events::EventQueue`]
//! is the implementation that copies and queues. The ids are the service's, never reused; a call
//! about an id the engine does not know is ignored.

/// `WEBD_LOST_*`: why the engine gave a view up.
pub const LOST_REFUSED: i32 = 1;
pub const LOST_CRASHED: i32 = 2;
pub const LOST_MEMORY: i32 = 3;
pub const LOST_OTHER: i32 = 4;

/// What a light view is made with (`webd_view_config` with `light = 1`). The service builds it from
/// a validated `LightConfig`; the engine reads it once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineConfig {
    /// JavaScript runs in the page.
    pub scripts: bool,
    /// Pixels, already within `limits`.
    pub width: i32,
    pub height: i32,
    /// Views of one owner with the same non-zero group share a web process. Derived by the service
    /// from the kernel-stamped caller; the engine cannot tell owners apart.
    pub group: i32,
    /// 0 follows the page, 1 dark.
    pub color_scheme: i32,
    /// CSS pixels; 0 keeps WebKit's default.
    pub default_font_size: i32,
    /// Extra CSS for every page of the view; empty for none.
    pub user_style: String,
    /// `None`: a light view. `Some`: a full view (`webd_view_config` with `light = 0`), with what
    /// the service decided for it from its `ViewConfig` and the scope it came through.
    pub full: Option<FullConfig>,
}

/// `webd_view_config.features` (the engine's `LZW_FEATURE_*` values).
pub const FEATURE_PAGE_UI: i32 = 1;
pub const FEATURE_DIALOGS: i32 = 2;
pub const FEATURE_DOWNLOADS: i32 = 4;
pub const FEATURE_POLICY: i32 = 8;
pub const FEATURE_NOTIFICATIONS: i32 = 64;
pub const FEATURE_BLOCKING: i32 = 128;

/// What a full view is made with, beyond the light fields (`webd_view_config`'s appended part).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FullConfig {
    /// `FEATURE_*` bits; `FEATURE_POLICY` always.
    pub features: i32,
    /// The absolute directory of the profile's store, or empty for a store in memory of the view's
    /// group.
    pub profile: String,
    /// The store keeps the page's data in memory (the directory then holds only the rule store).
    pub memory_only: bool,
    /// The view sends what changed (deltas, bands), not only whole frames.
    pub deltas: bool,
    /// Appended to the user agent; printable ASCII, at most 64 bytes.
    pub user_agent_suffix: String,
    /// Replaces the user agent (printable ASCII, at most 256 bytes); empty keeps WebKit's, with the
    /// suffix if any. Never together with a suffix.
    pub user_agent: String,
    /// No network at all (a `DataOnly` view): the light views' network lock is attached too, so a
    /// sub-resource, a fetch or a socket of the page reaches nothing, not only its navigations.
    pub no_network: bool,
    /// A private window's store (`WEBD_STORE_PRIVATE`): in memory like the client's own profile, but never
    /// the same store or web process as the client's other views, which share the client's group.
    pub private_store: bool,
}

/// What the service asks the engine. `&self`: the C functions take no state and the recording
/// double locks its own.
pub trait Engine {
    /// `webd_engine_view_create`. Asynchronous: success is the first state event for the id; a refused
    /// configuration is a view-lost event.
    fn view_create(&self, view: u64, config: &EngineConfig);
    /// `webd_engine_view_destroy`: no event about the id follows once the engine has run it.
    fn view_destroy(&self, view: u64);
    /// `webd_engine_load_html`: `html` is UTF-8 and copied before the call returns.
    fn load_html(&self, view: u64, html: &[u8], base_url: &str);
    fn resize(&self, view: u64, width: i32, height: i32);
    fn set_visible(&self, view: u64, visible: bool);
    /// The next frame is a whole one.
    fn refresh(&self, view: u64);
    /// `kind` 0 move, 1 press, 2 release; `button` 0 left, 1 middle, 2 right; `mods` bit 0 shift,
    /// 1 control, 2 alt, 3 meta; coordinates are view pixels.
    fn mouse(&self, view: u64, kind: i32, button: i32, x: i32, y: i32, mods: u32);
    fn wheel(&self, view: u64, x: i32, y: i32, ticks_x: i32, ticks_y: i32, mods: u32);
    /// `keysym` is the X11 keysym; `text` the character it types (empty for none).
    fn key(&self, view: u64, press: bool, keysym: u32, text: &str, mods: u32);
    fn focus(&self, view: u64, focused: bool);
    /// `webd_engine_quit`: ends the run loop; the process then exits.
    fn quit(&self);
    /// The page processes' ids as last seen, `(web, network)`; 0 for none.
    fn children(&self) -> (i32, i32);

    // ---- full views (webd_abi.h, P2 slice 3). A light view never gets these. The defaults do
    // nothing, for engines (test doubles) that serve light views only. ----

    /// `webd_engine_load`: the service has checked `url` against the view's policy.
    fn load(&self, _view: u64, _url: &str) {}
    fn reload(&self, _view: u64) {}
    fn stop(&self, _view: u64) {}
    fn back(&self, _view: u64) {}
    fn forward(&self, _view: u64) {}
    /// Save `url` as a download.
    fn download(&self, _view: u64, _url: &str) {}
    fn download_cancel(&self, _view: u64, _download: u64) {}
    fn dialog_reply(&self, _view: u64, _dialog: u64, _accepted: bool, _value: &str) {}
    /// The answer to a navigation the engine holds (`navigate` said ask).
    fn navigation_answer(&self, _view: u64, _request: u64, _allow: bool) {}
    fn notify_shown(&self, _id: u64) {}
    fn notify_clicked(&self, _id: u64) {}
    fn notify_closed(&self, _id: u64) {}
    /// 1 allow, 0 block, -1 dismiss: settles `view`'s pending requests for `origin`.
    fn notify_permission(&self, _view: u64, _origin: &str, _answer: i32) {}
    fn blocking_load(&self, _view: u64, _list: &str) {}
    fn blocking_compile(&self, _view: u64, _list: &str, _json: &[u8]) {}
    fn blocking_forget(&self, _view: u64, _list: &str) {}

    // ---- P4 (LazyWeb+). ----

    /// `webd_engine_recover`: end the view's web process; with `reload` the page loads again in a
    /// fresh one, without it the engine reports the page as ended (`LOST_OTHER`). The engine refuses
    /// while other views share the process.
    fn recover(&self, _view: u64, _reload: bool) {}
    /// `webd_engine_recover_group`: end the web process of the view's group even though other views
    /// share it; every view in it is reported as ended by request (`LOST_OTHER`).
    fn recover_group(&self, _view: u64) {}
    /// `webd_engine_set_user_agent`: from the view's next load on, `agent` is its user agent; empty
    /// restores the one the view was made with.
    fn set_user_agent(&self, _view: u64, _agent: &str) {}
    /// `webd_engine_notify_forget`: `origin` is `default` again for the page (WebKit's record is the
    /// context's, host-wide). `view` is the one the request came through.
    fn notify_forget(&self, _view: u64, _origin: &str) {}
}

/// What the engine tells the service, one method per `webd_ui_*` callback. Called on WebKit's main
/// thread; the borrowed data is valid only until the call returns, so an implementation copies.
pub trait EngineEvents: Send + Sync {
    /// `webd_ui_state`: the page's title, whether a load is in progress, how many loads have finished,
    /// and `""` or a short reason a load failed.
    fn state(&self, view: u64, title: &str, loading: bool, finished: i32, error: &str);
    /// `webd_ui_frame`: a whole opaque RGBA frame, `width * 4` bytes per row, top row first.
    fn frame(&self, view: u64, rgba: &[u8], width: i32, height: i32);
    /// `webd_ui_scroll`.
    fn scroll(&self, view: u64, x: i32, y: i32, at_bottom: bool);
    /// `webd_ui_link_clicked`: nothing loaded; `url` is absolute.
    fn link_clicked(&self, view: u64, url: &str);
    /// `webd_ui_clipboard_write`.
    fn clipboard_write(&self, view: u64, text: &str);
    /// `webd_ui_view_lost` (`LOST_*`): the id is dead afterwards.
    fn view_lost(&self, view: u64, reason: i32);
    /// `webd_ui_say`: one marker line without prefix or newline, from any thread.
    fn say(&self, line: &str);
    /// `webd_ui_user_memory`: the user's memory charge and limit in bytes, `None` when unknown.
    fn user_memory(&self) -> Option<(u64, u64)>;
}
