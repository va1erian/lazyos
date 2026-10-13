// The C ABI between WebKit's UI process (engine.cpp, main.cpp, clients.cpp, policy.cpp, uiclient.cpp)
// and a window (Rust static libraries: xui-app/crates/webkit is the part every window shares,
// os.lazy.webkit's window lives beside it, LazyWebPlus's is xui-app/webplus):
// docs/webkit-port-plan.md W5 and docs/lazywebplus.md.
//
// Two threads. WebKit's run loop owns the process's main thread; main() starts a
// second one that calls lzw_ui_main() and stays in it until the window closes.
// Everything the window asks of WebKit (lzw_engine_*, implemented in engine.cpp)
// only queues work for the run loop and returns, so it may be called from the
// window's thread at any rate. Everything WebKit says (lzw_ui_*, implemented in Rust)
// is safe to call from the run loop's thread and returns without waiting for the window,
// except the few that must decide on the spot (lzw_ui_navigate, lzw_ui_download_start): those
// are pure decisions and a file name, never a call into the window's thread.
//
// What the UI process installs depends on lzw_ui_features(): a window that asks for nothing gets
// the page, its state and its input, as in W5.

#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// ---- features (lzw_ui_features) ----

// The page's UI client: lzw_ui_hover, lzw_ui_cursor, lzw_ui_new_window.
#define LZW_FEATURE_PAGE_UI 1
// JavaScript alert, confirm and prompt: lzw_ui_dialog / lzw_engine_dialog_reply.
#define LZW_FEATURE_DIALOGS 2
// Attachments and responses the page cannot show become downloads: lzw_ui_download_*.
#define LZW_FEATURE_DOWNLOADS 4
// Every navigation asks lzw_ui_navigate.
#define LZW_FEATURE_POLICY 8
// The window prints the markers that name pages (LOAD, TITLE, ...): this process does not.
#define LZW_FEATURE_OWN_MARKERS 16
// A website data store in the directory lzw_ui_profile_dir() names.
#define LZW_FEATURE_PROFILE 32

// ---- decisions (lzw_ui_navigate) ----

// Navigation types (WKFrameNavigationType, WebKit's own numbers).
#define LZW_NAV_LINK_CLICKED 0
#define LZW_NAV_FORM_SUBMITTED 1
#define LZW_NAV_BACK_FORWARD 2
#define LZW_NAV_RELOAD 3
#define LZW_NAV_FORM_RESUBMITTED 4
#define LZW_NAV_OTHER 5

// What lzw_ui_navigate answers.
#define LZW_VERDICT_ALLOW 0
#define LZW_VERDICT_DENY 1
#define LZW_VERDICT_HANDOFF 2

// Pointer shapes (lzw_ui_cursor).
#define LZW_CURSOR_DEFAULT 0
#define LZW_CURSOR_HAND 1
#define LZW_CURSOR_TEXT 2
#define LZW_CURSOR_WAIT 3
#define LZW_CURSOR_HIDDEN 4
#define LZW_CURSOR_RESIZE_H 5
#define LZW_CURSOR_RESIZE_V 6
#define LZW_CURSOR_RESIZE_NWSE 7
#define LZW_CURSOR_RESIZE_NESW 8

// JavaScript dialogs (lzw_ui_dialog).
#define LZW_DIALOG_ALERT 0
#define LZW_DIALOG_CONFIRM 1
#define LZW_DIALOG_PROMPT 2

// ---- implemented by the window (Rust) ----

// Which features this window uses (LZW_FEATURE_*). Called before the window thread exists.
int lzw_ui_features(void);

// The prefix of every marker the UI process prints ("WEBKIT:W5", "WEBPLUS"), a static string.
const char* lzw_ui_marker_prefix(void);

// The directory for cookies and site data (LZW_FEATURE_PROFILE), created, or NULL for none. A
// static string, valid for the process's life. Called on the main thread before WebKit starts.
const char* lzw_ui_profile_dir(void);

// Whether this image carries the UI probe (LAZYOS_UI_PROBE=1): session scripts read markers that
// name pages (LOAD, TITLE, JS, CLICK, INPUT, NAV), which a shipped browser does not print.
int lzw_ui_probe_enabled(void);

// The calling user's user-memory charge and limit in bytes, as the kernel counts them (the length
// of every mapping, not the pages touched). Returns 0 on success.
int lzw_ui_user_memory(uint64_t* used, uint64_t* limit);

// Runs the window until it is closed; returns the process exit status. argv[1], if
// it is not an option, is the first address to open.
int lzw_ui_main(int argc, const char* const* argv);

// WebKit painted a frame: exactly width * height * 4 bytes of opaque RGBA, top row first.
// The bytes are copied before the call returns, so the pointer need not outlive the call (the
// engine reuses its buffer for the next frame). A non-positive size or a null pointer is ignored.
void lzw_ui_frame(const uint8_t* rgba, int width, int height);

// The page's state changed (UTF-8, never null). progress is in thousandths. error is "" or what
// the window shows in its status line: a load that failed (the page on show stays), or, with
// crashed set, the page's process is gone (the window blanks the page until the next load).
// A load the user stopped is not an error. url is the address on show (the page being loaded or
// the committed one); committed_url is the page that is on screen ("" before the first commits).
// secure: the committed page came over HTTPS (a failed verification is a load error, never a
// page). finished counts the navigations that finished loading since the process started: a
// load that ends without it growing was stopped, failed, or became a download.
void lzw_ui_state(const char* url, const char* title, int loading, int can_back, int can_forward, int progress,
    const char* error, int crashed, const char* committed_url, int secure, int finished);

// LZW_FEATURE_POLICY: a navigation is about to start. kind: LZW_NAV_*. user_gesture: a click, a
// key or a typed address started it (WKNavigationActionHasUnconsumedUserGesture), not a script's
// timer, a load handler or a redirect. committed_url: what is on screen. Returns LZW_VERDICT_*;
// anything but ALLOW means the UI process does not load it (the window acts on it). Runs on the
// run loop: it must not block.
int lzw_ui_navigate(const char* url, int kind, int user_gesture, const char* committed_url);

// LZW_FEATURE_PAGE_UI: the pointer is over a link (its absolute URL), or no longer ("").
void lzw_ui_hover(const char* url);
// LZW_FEATURE_PAGE_UI: the page wants this pointer shape (LZW_CURSOR_*).
void lzw_ui_cursor(int shape);
// LZW_FEATURE_PAGE_UI: a page asked for a new window (target=_blank, window.open). The UI process
// opens none; the window decides (LazyWebPlus loads url in its one view when a gesture started it).
void lzw_ui_new_window(const char* url, int user_gesture);

// LZW_FEATURE_DIALOGS: the page opened a JavaScript dialog and waits. Returns at once; the window
// answers later with lzw_engine_dialog_reply(id, ...). default_value is the prompt's, else "".
void lzw_ui_dialog(uint64_t id, int kind, const char* message, const char* default_value);

// LZW_FEATURE_DOWNLOADS: a download began. The window picks the file: it writes the path the engine
// saves to (a ".part" file in the user's Downloads folder, NUL-terminated, within path_size bytes)
// into path and returns 0, or returns non-zero to refuse. total is the announced size, or -1.
int lzw_ui_download_start(uint64_t id, const char* url, const char* suggested_name, int64_t total, char* path,
    size_t path_size);
// Bytes written so far; total as in lzw_ui_download_start.
void lzw_ui_download_progress(uint64_t id, int64_t received, int64_t total);
// Every byte is in the file.
void lzw_ui_download_done(uint64_t id);
// The download failed or was cancelled; reason says why.
void lzw_ui_download_fail(uint64_t id, const char* reason);

// ---- implemented by the engine (C++) ----

void lzw_engine_load(const char* url);
void lzw_engine_reload(void);
void lzw_engine_stop(void);
void lzw_engine_back(void);
void lzw_engine_forward(void);

// The page view's size in pixels changed.
void lzw_engine_resize(int width, int height);

// kind: 0 down, 1 up, 2 move. button: -1 none, 0 left, 1 middle, 2 right (WKEventMouseButton).
// mods: WKEventModifiers (shift 1, control 2, alt 4, meta 8).
void lzw_engine_mouse(int kind, int button, int x, int y, uint32_t mods);

// A wheel step at (x, y), in xui's convention: ticks_y > 0 scrolls up, ticks_x > 0 scrolls right
// (WebKit's own is positive = up and positive = left: the engine flips x).
void lzw_engine_wheel(int x, int y, int ticks_x, int ticks_y, uint32_t mods);

// A key press or release. keysym is an X11 key symbol (libwpe's key codes); text is what
// the key types ("" for a key that types nothing).
void lzw_engine_key(int press, uint32_t keysym, const char* text, uint32_t mods);

void lzw_engine_focus(int focused);

// Saves url as a download, as if the server had sent it as an attachment (Save Page As). The
// download is reported through lzw_ui_download_*.
void lzw_engine_download(const char* url);
// Ends download id (the window removes the .part file).
void lzw_engine_download_cancel(uint64_t id);
// Answers the dialog id lzw_ui_dialog announced: accepted is OK, value the prompt's text.
void lzw_engine_dialog_reply(uint64_t id, int accepted, const char* value);

// Creates the context, the page and the view; must run on the main thread, before
// WKRunLoopRunMain(). Returns 0 on success. With LZW_FEATURE_PROFILE the website data store lives in
// lzw_ui_profile_dir().
int lzw_engine_start(void);

// Ends WKRunLoopRunMain() (callable from any thread).
void lzw_engine_quit(void);

// After WKRunLoopRunMain() returned, on the main thread: ends the WebProcess and the
// NetworkProcess (SIGTERM, then SIGKILL after a grace period), waits for them and prints
// <prefix>:CHILD:<web|network>:<pid>:GONE|ALIVE for each.
int lzw_engine_shutdown(void);

#ifdef __cplusplus
}
#endif
