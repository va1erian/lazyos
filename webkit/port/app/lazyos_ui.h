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
// The Notifications API: WebKit's provider and the page's permission requests,
// lzw_ui_notify_* / lzw_engine_notify_*.
#define LZW_FEATURE_NOTIFICATIONS 64
// Content blocking (issue #845): rule lists compiled into a store below the profile directory and
// attached to the page; lzw_ui_blocking decides per main-frame navigation whether they apply.
// Needs LZW_FEATURE_PROFILE (the store lives in the profile) and a WebKit built with content
// extensions (ENABLE_CONTENT_EXTENSIONS, OptionsLazyOS.cmake).
#define LZW_FEATURE_BLOCKING 128

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

// What became of a rule list (lzw_ui_blocking_list).
#define LZW_BLOCK_READY 0         // stored, attached to the page
#define LZW_BLOCK_NOT_STORED 1    // lzw_engine_blocking_load: nothing stored under that id
#define LZW_BLOCK_STALE 2         // stored by an older WebKit format: compile it again
#define LZW_BLOCK_COMPILE_FAILED 3 // the JSON was refused (WebKit's parser; one bad rule fails a list)
#define LZW_BLOCK_UNAVAILABLE 4   // no store: no profile, or no content extensions in this build

// JavaScript dialogs (lzw_ui_dialog).
#define LZW_DIALOG_ALERT 0
#define LZW_DIALOG_CONFIRM 1
#define LZW_DIALOG_PROMPT 2

// ---- implemented by the window (Rust) ----

// Which features this window uses (LZW_FEATURE_*). Called before the window thread exists.
int lzw_ui_features(void);

// The prefix of every marker the UI process prints ("WEBKIT:W5", "WEBPLUS"), a static string.
const char* lzw_ui_marker_prefix(void);

// Writes "<prefix>:<line>" as one line on the window's trace channel (one write, so another
// thread's line cannot land inside it; LAZYOS_TRACE=0 and the trace mask apply). Any thread; `line`
// is copied before this returns and has no newline.
void lzw_ui_say(const char* line);

// The directory for cookies and site data (LZW_FEATURE_PROFILE), created, or NULL for none. A
// static string, valid for the process's life. Called on the main thread before WebKit starts.
const char* lzw_ui_profile_dir(void);

// Whether the page's data (cookies, the HTTP cache, site storage) is kept in memory only, gone when
// the browser quits: WebKit's non-persistent data store, though the profile directory still holds
// the content-blocking store (issue #940: a profile on a USB stick stalled the network process on
// every write). Called on the main thread before WebKit starts.
int lzw_ui_profile_ephemeral(void);

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

// WebKit painted part of the page. `rgba` is a whole frame, laid out as for lzw_ui_frame, of which
// only the rectangles are read (and copied before the call returns). The picture the window has
// is first moved, if `scroll` is not null: six ints, x, y, width, height of the area that moved
// and dx, dy; what was inside the area is dx, dy further on, what leaves it is gone and what it
// uncovers is stale until a rectangle covers it. Then each of the `count` rectangles (four ints
// each, x, y, width, height, inside the frame, at most 8) replaces what it covers. Returns 1 when
// the window cannot take it (it has no picture to change, is too far behind, or the arguments
// make no sense): the next frame to send is a whole one, lzw_ui_frame.
int lzw_ui_frame_delta(const uint8_t* rgba, int width, int height, const int* scroll, const int* rects, int count);

// The page's process painted rows of the page outside the view ahead of a scroll (an overscan band,
// WebKit patch 0030): the rows doc_y .. doc_y + height, painted at the horizontal scroll offset doc_x,
// as width * height opaque RGBA pixels, top row first, copied before the call returns.
void lzw_ui_band(int doc_x, int doc_y, int width, int height, const uint8_t* rgba);

// The main frame's scroll offset (page pixels) of the pixels the next lzw_ui_frame or lzw_ui_frame_delta
// carries: what WKPageSetScrollOffsetCallback last said before the update they come from. Call it just
// before posting; it belongs to that one post. The window shows its picture ahead of the page by what
// the wheel has asked for and the offset says the page has not done yet (speculate.rs). `at_bottom` is
// non-zero when the page cannot scroll further down from that offset (WKPageIsPinnedToBottomSide at the
// update): the window then shows nothing ahead downward, never rows past the end of the document.
void lzw_ui_scroll_offset(int x, int y, int at_bottom);

// What the page changed, kept by the window's library in the form a paint needs (the rules are
// Rust, tested against a model of WebKit's backing store: xui-app/crates/webkit/src/damage.rs).
// Only the engine's thread calls these.
// The view has this size: nothing painted before it fits.
void lzw_paint_resize(int width, int height);
// All of the view must be painted and sent whole.
void lzw_paint_full(void);
// A rectangle changed (the view was reported as needing display).
void lzw_paint_rect(int x, int y, int width, int height);
// One update from the page's process: the area sx, sy, sw, sh moved by dx, dy (an empty area or a
// zero offset: nothing moved) and the `count` rectangles (four ints each) were repainted.
void lzw_paint_update(int sx, int sy, int sw, int sh, int dx, int dy, const int* rects, int count);
// What the next paint has to do, forgotten once asked. Returns LZW_PAINT_NOTHING, or LZW_PAINT_FULL
// (paint all of the view, send it with lzw_ui_frame), or LZW_PAINT_DELTA: `scroll` (six ints, all
// zero when nothing moved) and the `*count` rectangles written to `rects` (four ints each, room
// for `max`) are to be sent with lzw_ui_frame_delta after the rectangles were painted.
#define LZW_PAINT_NOTHING 0
#define LZW_PAINT_FULL 1
#define LZW_PAINT_DELTA 2
int lzw_paint_plan(int* scroll, int* rects, int max, int* count);

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

// The page's WebProcess stopped answering WebKit's responsiveness pings (responsive 0), or answers
// again (1), or is gone (1: there is nothing left to wait for). The window may offer a way out
// (lzw_engine_recover). Runs on the run loop and must not block.
void lzw_ui_page_responsive(int responsive);

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

// LZW_FEATURE_NOTIFICATIONS: the decisions the window kept about which sites may notify, read when
// WebKit starts a page's process and again at start-up. `index` counts from 0; origin gets the
// site as WebKit spells it ("https://example.com", the port only when it is not the scheme's own),
// NUL-terminated within origin_size bytes. Returns 1 allowed, 0 blocked, -1 past the end, -2 for
// a row that does not fit (the caller skips it). Called on the run loop and on the main thread
// before the window exists: it reads a mutex-guarded list and never waits for the window.
int lzw_ui_notify_permission_at(size_t index, char* origin, size_t origin_size);

// LZW_FEATURE_NOTIFICATIONS: a page asked to show notification `id` (WebKit's own id, nonzero).
// origin is the page's security origin as WebKit writes it; page_url the address of the page that
// asked; title, body and tag are the page's own words, untrusted. The window answers with
// lzw_engine_notify_shown when it is up, and says nothing when it drops it.
void lzw_ui_notify_show(uint64_t id, const char* origin, const char* page_url, const char* title,
    const char* body, const char* tag);
// The page took notification `id` down itself (Notification.close()).
void lzw_ui_notify_cancel(uint64_t id);
// The page's object for notification `id` is gone (its document was): a click can no longer reach
// a script, and the toast, if any, stays.
void lzw_ui_notify_gone(uint64_t id);
// A page called Notification.requestPermission() for `origin` and waits. user_gesture is 1: WebKit
// itself refuses the call without a user gesture (a transient activation) before the UI process
// hears of it; the window looks at the input it forwarded as well. The answer is
// lzw_engine_notify_permission(origin, ...).
void lzw_ui_notify_permission_request(const char* origin, int user_gesture);
// The pending permission requests of `origin` were refused by the UI process itself (their page
// started another navigation, or its process went): they need no answer.
void lzw_ui_notify_permission_gone(const char* origin);

// LZW_FEATURE_BLOCKING: whether the rule lists apply to a page whose main-frame address is url (the
// user's switch, their exceptions, a one-shot "reload without blocking"). Called on the run loop for
// every main-frame navigation, redirects included, before the load starts; a pure decision that must
// not block. Non-zero: apply.
int lzw_ui_blocking(const char* url);
// LZW_FEATURE_BLOCKING: what became of rule list id after lzw_engine_blocking_load or _compile
// (LZW_BLOCK_*). Attached to the page already when it is LZW_BLOCK_READY.
void lzw_ui_blocking_list(const char* id, int status);
// LZW_FEATURE_BLOCKING: the number of loads the lists blocked on the page on show, since it
// committed (0 when a page commits).
void lzw_ui_blocked(int count);

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
// The same event, handed to the page at once: on WebKit's main thread only (the window's merged
// pointer moves, issue #890, are delivered from a task lzw_engine_run_on_main queued).
void lzw_engine_mouse_now(int kind, int button, int x, int y, uint32_t mods);

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

// LZW_FEATURE_NOTIFICATIONS. Notification `id` is on screen: the page gets its `show` event.
void lzw_engine_notify_shown(uint64_t id);
// The person clicked notification `id`: the page's `click` handler runs, when its document is
// still there.
void lzw_engine_notify_clicked(uint64_t id);
// Notification `id` went away without the page asking (dismissed, timed out): the page gets its
// `close` event.
void lzw_engine_notify_closed(uint64_t id);
// The answer to lzw_ui_notify_permission_request: answer 1 allows `origin`, 0 blocks it (both are
// remembered by WebKit for the pages of that origin: Notification.permission reads `granted` or
// `denied`), -1 refuses the pending requests and remembers nothing (the site may ask again).
void lzw_engine_notify_permission(const char* origin, int answer);
// The person revoked the decision about `origin`: Notification.permission is `default` again.
void lzw_engine_notify_forget(const char* origin);

// Runs task(context) once on WebKit's main thread, the process's first thread, and returns at once
// (callable from any thread). A Messenger service names a caller by its task, and the shell's
// notification service refuses a thread that is not the process's first (docs/lazywebplus.md): a
// window's calls to it are made from here.
void lzw_engine_run_on_main(void (*task)(void* context), void* context);

// LZW_FEATURE_BLOCKING: attaches rule list `id` from the store (a list compiled earlier, so a start
// pays no compile). The result comes as lzw_ui_blocking_list.
void lzw_engine_blocking_load(const char* id);
// LZW_FEATURE_BLOCKING: compiles `json` (a WebKit content-rule list, NUL-terminated and copied) as
// list `id`, stores it and attaches it, replacing a list of that id. Compiles on WebKit's work queue;
// the result comes as lzw_ui_blocking_list.
void lzw_engine_blocking_compile(const char* id, const char* json);
// LZW_FEATURE_BLOCKING: detaches list `id` and removes it from the store.
void lzw_engine_blocking_forget(const char* id);

// Creates the context, the page and the view; must run on the main thread, before
// WKRunLoopRunMain(). Returns 0 on success. With LZW_FEATURE_PROFILE the website data store lives in
// lzw_ui_profile_dir().
int lzw_engine_start(void);

// Ends WKRunLoopRunMain() (callable from any thread).
void lzw_engine_quit(void);

// After WKRunLoopRunMain() returned, on the main thread: ends every engine process the launcher
// started (SIGTERM, then SIGKILL of each one's process group, retried every 100 ms for up to 10 s),
// waits for them and prints <prefix>:CHILD:<web|network>:<pid>:GONE|ALIVE:term=<rc>:kill=<rc>:wait=<rc>
// for each. Returns how many are left.
int lzw_engine_shutdown(void);

// The ids of the page's WebProcess and of the NetworkProcess as WebKit last said them (0: none
// yet). Callable from any thread: the window's heartbeat reads their CPU use.
void lzw_engine_children(int* web, int* network);

// The page's WebProcess does not answer (it is stuck) and the person chose a way out: its process is
// ended, and with reload non-zero the page is loaded again in a fresh one; else the page stays
// stopped (lzw_ui_state's `crashed`) until the next load.
void lzw_engine_recover(int reload);

// WebKit's launcher reports every engine process it starts, collects or kills here (a weak
// symbol there: the stand-alone programs that link WebKit without this file print to stderr).
// type: "web" or "network"; event: "launch", "fail" (detail: errno), "exit" (detail: status),
// "signal" (detail: signal), "vanish", "terminate" (detail, detail2: errno of kill(-pid) and
// of kill(pid), 0 when sent, -1 when not tried). Any thread.
void lzw_launcher_event(const char* type, int pid, const char* event, int detail, int detail2);

// Implemented by WebKit's launcher (ProcessLauncherLazyOS.cpp): collects, on the calling thread and under
// the launcher's lock (the one that orders every reap against every kill), the engine processes that are
// over, and reports each through lzw_launcher_event. WebKit's main thread calls it when WebKit says a
// process ended: until the kernel makes children per process (issue #859) only the thread that forked a
// child can wait for it.
void lzw_launcher_reap(void);

// Implemented by WebKit's launcher: sends `signal` to every live child under the launcher's lock (to the
// child's process group first when `group` is non-zero, then to the process). Fills `pids` and `codes` (0:
// sent, else the errno negated) for up to `capacity` children and returns how many are live. The shutdown's
// only way to signal: an id that was collected (and may be reused) is never named.
int lzw_launcher_signal(int signal, int group, int* pids, int* codes, int capacity);

// WebKit keeps the proxy of every web process that ended alive (patch 0021: destroying it aborts the UI
// process); `kept` counts them since the start. Prints PROCESS:KEPT:<pid>:<kept>. Weak in WebKit.
void lzw_proxy_kept(int pid, int kept);

// Whether WebKit's run loop has begun to turn (any thread): a ping posted before it would be late
// through no fault of the loop.
int lzw_engine_running(void);

#ifdef __cplusplus
}
#endif
