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
