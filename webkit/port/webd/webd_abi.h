// The C ABI between webd's WebKit half (C++, webkit/port/webd/abi.cpp over the engine of webkit/port/app:
// one Host, any number of Views) and webd's service half (Rust, xui-app/crates/webd). docs/webd-plan.md
// sections 3.2 and 3.6, phase P2.
//
// P2 slice 1 wrote the LIGHT views' part; slice 3 appended FULL views (navigation, profiles, dialogs, downloads,
// notifications, blocking, deltas) at the end of each struct and of the file. Both halves are written against
// this file; change it only together with both, append-only: every struct starts with its size.
//
// Threads. WebKit's run loop owns ONE thread of the process, and the service another: webd runs the service on
// the process's first thread (init's lifecycle Watch and the shell's services want that) and the run loop on a
// thread it makes; webd-abi-test runs the loop on the main thread and the test on a second. Below, "main
// thread" means the thread that runs the loop.
//  * webd_engine_*  (implemented in C++): callable from ANY thread at any rate; each only queues work for the
//    run loop and returns. Nothing in it waits for WebKit.
//  * webd_ui_*      (implemented in Rust): called on WebKit's main thread, except webd_ui_say (any thread);
//    each returns without waiting for the service (it queues or copies), and the pointers it is given are
//    valid only until it returns.
//
// Ids. The SERVICE chooses every view id (a non-zero u64, never reused within a process) and passes it in
// webd_engine_view_create; the engine uses it in every webd_ui_* call about that view. A call about an id
// the engine does not know (destroyed, never made, or not yet created on the main thread) is ignored.
//
// Pixels. Every frame is opaque RGBA, 4 bytes per pixel, top row first, stride width * 4. A light view, and a
// full view made with deltas = 0, gets WHOLE frames only: the engine paints the whole area whenever anything in
// it changed, at most once per paint interval. A full view with deltas = 1 gets what changed (webd_paint_*).

#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// ---- view configuration ----

// Every field is read when the view is created and never again. UTF-8, NUL-terminated strings; NULL means "".
typedef struct webd_view_config {
    // sizeof(webd_view_config) as the caller knows it. The engine reads fields only within it (growth).
    uint32_t struct_size;
    // 1: a LIGHT view (docs/webd-plan.md 3.6): a non-persistent data store of its own, no profile directory, no
    // network of any kind (a load other than a LoadHtml or the data:, blob: and about:blank schemes is refused
    // in the engine, not just not requested), navigation locked (a link click, a form submit and a meta
    // refresh become webd_ui_link_clicked and nothing loads), and none of a browser's clients: no UI client
    // (no hover, cursor, window.open), no dialogs, downloads, notifications, content blocking or navigation
    // policy. 0: a FULL view, configured by the fields appended below (P2 slice 3).
    int32_t light;
    // 1: JavaScript runs in the page. A light view's default is 0 and the service passes 1 only when the caller
    // asked for it. Script never gets network, storage or windows in a light view.
    int32_t scripts;
    // Size in pixels (not scaled: slice 1 has no device pixel ratio). Clamped by the engine to 1..8192.
    int32_t width;
    int32_t height;
    // Views of one owner with the same non-zero group share one WebProcess (engine.h, ViewConfig::group).
    // The SERVICE derives it from the kernel-stamped caller, never from a wire field; the engine cannot tell
    // owners apart. 0: a process of its own.
    int32_t group;
    // 0 follow the page (light), 1 dark (prefers-color-scheme: dark). Best effort: the engine may ignore it
    // in slice 1 if WebKit's C API has no way; it must say so in a marker, not fail.
    int32_t color_scheme;
    // The page's default (and minimum) font size in CSS pixels; 0 keeps WebKit's default. Best effort.
    int32_t default_font_size;
    // Extra CSS applied to every page of the view (the caller's `user_style`), or NULL. Best effort (a user
    // style sheet in the view's user content controller); at most 64 KiB, the service checks.
    const char* user_style;

    // ---- appended in P2 slice 3 (FULL views, light = 0); a light view ignores them ----

    // The WEBD_FEATURE_* bits this view uses: what is installed for it and nothing else (engine.h,
    // ViewConfig::features). WEBD_FEATURE_POLICY is always set by the service for a full view.
    int32_t features;
    // The absolute directory of the view's profile (its data store, and the rule-list store of
    // WEBD_FEATURE_BLOCKING), created by the engine; NULL or "" for a store in memory of the view's own
    // group (engine.h: views of one group and one profile share a store and a web process).
    const char* profile;
    // 1: the store keeps the page's data in memory even with a profile directory (which then holds only the
    // rule-list store): WebKit's non-persistent data store.
    int32_t memory_only;
    // 1: the view sends what changed (webd_paint_*, webd_ui_frame_delta, webd_ui_band) rather than whole
    // frames only; 0: every frame is a whole one (webd_ui_frame), as for a light view.
    int32_t deltas;
    // Appended to the user agent, or NULL; at most 64 printable ASCII bytes (else ignored, CONFIG:IGNORED). The
    // page's agent becomes WebKit's, a space and the suffix (a custom user agent: WKPageSetApplicationNameForUserAgent
    // would add "/<WebKit version>"), and the view has no site-specific quirks, which on this port put WebKit's
    // fixed agent on every request whatever the page's own is (WebPagePlayStation.cpp).
    const char* user_agent_suffix;
    // 1: the light views' network lock (the content rule list that blocks every load but data:, blob: and about:,
    // light.cpp) is attached to this full view too: no subresource, script request or document load of the page
    // reaches a network, whatever webd_ui_navigate answers. A full view made with it waits for the lock like a
    // light view (refused if the lock failed). With WEBD_FEATURE_BLOCKING it is refused (view_lost(REFUSED)):
    // the blocking lists are rebuilt on the page's controller at every navigation and could drop the lock.
    int32_t no_network;
    // Which kind of store the view's data lives in (WEBD_STORE_*), appended with the 2026-10-22 review. Views join an
    // existing view's web process and data store only when the group, the profile directory, memory_only, no_network
    // AND this match: a client's own in-memory profile and the same client's private window are both "in memory,
    // no directory, one group", and without this field the second would be a related page of the first and share
    // its cookies and storage. Any value but the two below is a store of its own kind (never shared with 0 or 1).
    int32_t store_kind;
    // Replaces the user agent entirely, or NULL; at most 256 printable ASCII bytes (else ignored, CONFIG:IGNORED), appended
    // 2026-10-23 with the tabs work. The service never sets it together with user_agent_suffix. Like the suffix it
    // turns the view's site-specific quirks off (they put a fixed agent on every request), and it makes the view one
    // webd_engine_set_user_agent may change later.
    const char* user_agent;
} webd_view_config;

#define WEBD_STORE_OWN 0      // the client's own profile (default)
#define WEBD_STORE_PRIVATE 1  // a private window: in memory, shared only with the client's other private windows

// webd_view_config.features (the values are lazyos_ui.h's LZW_FEATURE_*, which abi.cpp asserts).
#define WEBD_FEATURE_PAGE_UI 1          // webd_ui_hover, webd_ui_cursor, webd_ui_new_window
#define WEBD_FEATURE_DIALOGS 2          // webd_ui_dialog / webd_engine_dialog_reply
#define WEBD_FEATURE_DOWNLOADS 4        // webd_ui_download_*
#define WEBD_FEATURE_POLICY 8           // webd_ui_navigate decides every navigation
#define WEBD_FEATURE_NOTIFICATIONS 64   // webd_ui_notify_* / webd_engine_notify_*
#define WEBD_FEATURE_BLOCKING 128       // webd_ui_blocking*, webd_engine_blocking_* (needs a profile directory)

// How the host starts (webd_engine_start_host). Read once.
typedef struct webd_host_config {
    uint32_t struct_size;
    // The directory the engine's processes start in and below which WebKit puts the data directories a
    // store does not name (LAZYOS_WEBKIT_PROFILE, patch 0015); created. NULL: none (light views only). The
    // children take their environment when they start and one NetworkProcess serves every profile, so it is
    // decided here, once, and no view changes it: curl's default cookie database is in memory, and every
    // persistent profile's cookies are in its own file (the store's cookie storage file, which the
    // NetworkProcess opens per session: NetworkSessionCurl.cpp), never in another profile's.
    const char* children_directory;
} webd_host_config;

// ---- the service asks the engine (any thread) ----

// Creates the WebKit Host (the context and its engine processes' launcher). Call once, on the process's main
// thread, before the run loop turns; returns 0 on success.
int webd_engine_start(void);

// Creates a view with this id on the run loop. Asynchronous: success is the first webd_ui_state for the id.
// A refused configuration (a profile the engine cannot make, a feature it lacks) is answered by webd_ui_view_lost(id, WEBD_LOST_REFUSED).
void webd_engine_view_create(uint64_t view, const webd_view_config* config);

// Ends a view and its page. No webd_ui_* call about the id follows once the engine has run this.
void webd_engine_view_destroy(uint64_t view);

// Loads the caller's HTML (`html`, `len` bytes of UTF-8, copied before the call returns) with this base URL
// ("about:blank" or an opaque origin; anything with a network or file scheme is refused: nothing loads and the
// next webd_ui_state carries a non-empty error). Replaces the page.
// A FULL view takes the client's own page too, under a base URL of the about:, data:, http: or https: scheme the
// service checked against the view's policy ("" is about:blank); any other (file:, ...) loads nothing and the
// next webd_ui_page_state carries an error. Its navigation asks webd_ui_navigate like any other.
void webd_engine_load_html(uint64_t view, const uint8_t* html, size_t len, const char* base_url);

// The view's size in pixels; the next frame has this size.
void webd_engine_resize(uint64_t view, int width, int height);

// A view that is not visible gets no paints (its page still runs).
void webd_engine_set_visible(uint64_t view, int visible);

// The next frame is a whole one (the service lost the last, or a slot was re-attached).
void webd_engine_refresh(uint64_t view);

// Input, as the engine's View methods take it (engine.h). kind: 0 move, 1 press, 2 release; button: 0 left,
// 1 middle, 2 right; mods: bit 0 shift, 1 control, 2 alt, 3 meta. Coordinates are view pixels.
void webd_engine_mouse(uint64_t view, int kind, int button, int x, int y, uint32_t mods);
void webd_engine_wheel(uint64_t view, int x, int y, int ticks_x, int ticks_y, uint32_t mods);
// keysym is the X11 keysym the display protocol carries; text is the character it types ("" for none).
void webd_engine_key(uint64_t view, int press, uint32_t keysym, const char* text, uint32_t mods);
void webd_engine_focus(uint64_t view, int focused);

// Runs `task(context)` on WebKit's main thread (the run loop), in order with the other calls.
void webd_engine_run_on_main(void (*task)(void* context), void* context);

// Ends the run loop (the process then exits). Any thread.
void webd_engine_quit(void);

// The process ids of the page processes as last seen (0: none); any thread. For the service's accounting.
void webd_engine_children(int* web, int* network);

// ---- the engine tells the service (WebKit's main thread) ----

// What the page reports about itself. `loading` 1 while a load is in progress; `finished` counts the loads
// that have finished (so a caller can tell the first paint of a LoadHtml from the old page's); `error` is "" or
// a short reason a load failed.
void webd_ui_state(uint64_t view, const char* title, int loading, int finished, const char* error);

// A whole frame (see "Pixels" above); copied by the callee before it returns.
void webd_ui_frame(uint64_t view, const uint8_t* rgba, int width, int height);

// The page is scrolled to (x, y) in document pixels; at_bottom 1 when it cannot scroll further.
void webd_ui_scroll(uint64_t view, int x, int y, int at_bottom);

// A link was clicked, a form submitted or a meta refresh fired: nothing loaded. `url` is absolute.
void webd_ui_link_clicked(uint64_t view, const char* url);

// The user copied text out of the page (Ctrl+C / Copy). Best effort in slice 1.
void webd_ui_clipboard_write(uint64_t view, const char* text);

// The view cannot go on: its web process ended, or the engine refused it. The id is dead afterwards (the
// engine has dropped it); the service answers every client call on it with ViewLost.
#define WEBD_LOST_REFUSED 1  // the configuration or the load was refused
#define WEBD_LOST_CRASHED 2  // the web process crashed or was killed
#define WEBD_LOST_MEMORY 3   // the web process ran out of memory
#define WEBD_LOST_OTHER 4
void webd_ui_view_lost(uint64_t view, int reason);

// One marker line from the engine, without a prefix or newline, written whole and from any thread.
void webd_ui_say(const char* line);

// The calling user's memory charge and limit in bytes; 0 on success, non-zero when unknown.
int webd_ui_user_memory(uint64_t* used, uint64_t* limit);

// =================================================================================================================
// P2 slice 3: FULL views (light = 0). Everything below is about full views only; a light view is as above. The
// semantics are lazyos_ui.h's (the single-page browser's ABI) with a view id on every call about a page.
// =================================================================================================================

// ---- the service asks the engine (any thread; each only queues work) ----

// webd_engine_start with a host configuration (above). Replaces webd_engine_start for a service that makes full
// views; calling either a second time fails. 0 on success.
int webd_engine_start_host(const webd_host_config* config);

// Navigation. The service has checked `url` against the view's policy; the engine asks webd_ui_navigate again
// when the navigation starts (as for every navigation).
void webd_engine_load(uint64_t view, const char* url);
void webd_engine_reload(uint64_t view);
void webd_engine_stop(uint64_t view);
void webd_engine_back(uint64_t view);
void webd_engine_forward(uint64_t view);
// Saves `url` as a download (Save Page As; WEBD_FEATURE_DOWNLOADS).
void webd_engine_download(uint64_t view, const char* url);
// Ends download `download` (the service removes the .part file once webd_ui_download_fail says so).
void webd_engine_download_cancel(uint64_t view, uint64_t download);
// Answers dialog `dialog` of webd_ui_dialog: accepted is OK, value the prompt's text.
void webd_engine_dialog_reply(uint64_t view, uint64_t dialog, int accepted, const char* value);
// Answers navigation `request` that webd_ui_navigate held (WEBD_VERDICT_ASK): non-zero loads it, 0 does not.
// A request the engine no longer holds (answered, dropped by a newer navigation, the view gone) is ignored.
void webd_engine_navigation_answer(uint64_t view, uint64_t request, int allow);
// Notifications (WEBD_FEATURE_NOTIFICATIONS): notification `id` is on screen / was clicked / went away.
void webd_engine_notify_shown(uint64_t id);
void webd_engine_notify_clicked(uint64_t id);
void webd_engine_notify_closed(uint64_t id);
// The answer to webd_ui_notify_permission_request for `origin` in `view`: 1 allow, 0 block, -1 dismiss. Only
// that view's pending requests are settled; WebKit's own record of the decision is the context's (one for every
// profile), so the service also filters webd_ui_notify_show by the view's profile.
void webd_engine_notify_permission(uint64_t view, const char* origin, int answer);
// Content blocking (WEBD_FEATURE_BLOCKING), on the view's profile's rule-list store: attach list `list` stored
// earlier, compile `json` (`len` bytes, copied) as `list` and attach it, or detach and remove it. The result
// comes as webd_ui_blocking_list.
void webd_engine_blocking_load(uint64_t view, const char* list);
void webd_engine_blocking_compile(uint64_t view, const char* list, const char* json, size_t len);
void webd_engine_blocking_forget(uint64_t view, const char* list);

// P4 (LazyWeb is a client of webd). The view's page process does not answer and the person chose a way out:
// the process is ended (View::recover). Non-zero `reload` loads the page again in a fresh process; 0 leaves the
// page stopped, which the engine reports as the page ended by request (webd_ui_view_lost, WEBD_LOST_OTHER). A
// process other views share is not ended (the engine says WEBPROCESS:RECOVER:SHARED and does nothing).
void webd_engine_recover(uint64_t view, int reload);
// Like webd_engine_recover(view, 0) for a view whose web process other views share (tabs of one site, one webd group):
// the process is ended (View::recoverGroup), no page is reloaded, and every view that ran in it is reported as ended by
// request (webd_ui_view_lost, WEBD_LOST_OTHER). The owner makes its views again. A view with no process is ignored.
void webd_engine_recover_group(uint64_t view);
// From the view's next load on `agent` (at most 256 printable ASCII bytes, else ignored) is its user agent; "" or NULL goes
// back to the one it was made with (user_agent, else WebKit's with user_agent_suffix, else WebKit's). Ignored for a view
// made with neither user_agent nor user_agent_suffix: WebKit's site-specific quirks are on for it and would put their fixed
// agent on every request whatever is set here (the service allows the call for such views never).
void webd_engine_set_user_agent(uint64_t view, const char* agent);
// Forgets the profile's decision about `origin`: the pages of that site get Notification.permission "default"
// again (WebKit's record is the context's, so host-wide, as for webd_engine_notify_permission). `view` is the
// full view the request came through; an unknown or light view ignores the call.
void webd_engine_notify_forget(uint64_t view, const char* origin);

// ---- the engine tells the service (WebKit's main thread) ----

// What a full view's page reports about itself (instead of webd_ui_state). Strings UTF-8, never NULL, valid for
// the call. A crashed page is webd_ui_view_lost, as for a light view.
typedef struct webd_page_state {
    uint32_t struct_size;
    const char* url;           // the address on show (being loaded, or committed)
    const char* title;
    int32_t loading;
    int32_t can_back;
    int32_t can_forward;
    int32_t progress;          // thousandths
    const char* error;         // "" or why a load failed (the page on show stays)
    const char* committed_url; // the page on screen ("" before the first commits)
    int32_t secure;            // the committed page came over verified HTTPS
    int32_t finished;          // navigations that finished loading since the view was made
} webd_page_state;
void webd_ui_page_state(uint64_t view, const webd_page_state* state);

// Painting a view with deltas = 1 (lazyos_ui.h's lzw_paint_* and lzw_ui_frame_delta, per view). The service
// keeps what changed between two paints (xui-app/crates/damage) and answers the plan; the engine then paints and
// posts. For such a view webd_ui_scroll is called before EVERY post (frame, delta) with the offset its pixels
// were painted at, not only when the offset changed.
#define WEBD_PAINT_NOTHING 0
#define WEBD_PAINT_FULL 1
#define WEBD_PAINT_DELTA 2
void webd_paint_resize(uint64_t view, int width, int height);
void webd_paint_full(uint64_t view);
void webd_paint_rect(uint64_t view, int x, int y, int width, int height);
void webd_paint_update(uint64_t view, int sx, int sy, int sw, int sh, int dx, int dy, const int* rects, int count);
int webd_paint_plan(uint64_t view, int* scroll, int* rects, int max, int* count);
// Part of the picture changed: `rgba` is laid out as a whole frame of which only the rectangles are read (and
// copied). `scroll`, if not NULL, is six ints (x, y, width, height of the area that moved, dx, dy), applied
// first. Returns non-zero when the service cannot take it: the next paint must be a whole frame.
int webd_ui_frame_delta(uint64_t view, const uint8_t* rgba, int width, int height, const int* scroll, const int* rects, int count);
// Rows of the page outside the view, painted ahead of a scroll (patch 0030): doc_y .. doc_y + height at the
// horizontal offset doc_x, width * height opaque RGBA pixels (width is the view's), copied.
void webd_ui_band(uint64_t view, int doc_x, int doc_y, int width, int height, const uint8_t* rgba);

// WEBD_FEATURE_PAGE_UI.
void webd_ui_hover(uint64_t view, const char* url);
// A pointer shape: lazyos_ui.h's LZW_CURSOR_* values.
void webd_ui_cursor(uint64_t view, int shape);
// A page asked for a new window; none opened.
void webd_ui_new_window(uint64_t view, const char* url, int gesture);
// The page's process stopped answering (0) or answers again (1).
void webd_ui_responsive(uint64_t view, int responsive);

// WEBD_FEATURE_DIALOGS: the page waits for webd_engine_dialog_reply(view, dialog, ...). kind: lazyos_ui.h's
// LZW_DIALOG_*.
void webd_ui_dialog(uint64_t view, uint64_t dialog, int kind, const char* message, const char* default_value);

// WEBD_FEATURE_DOWNLOADS: the service picks the file (NUL-terminated within path_size) and returns 0, or refuses
// with non-zero. A pure decision on the spot: it must not wait for the service thread.
int webd_ui_download_start(uint64_t view, uint64_t download, const char* url, const char* suggested_name, int64_t total, char* path, size_t path_size);
void webd_ui_download_progress(uint64_t view, uint64_t download, int64_t received, int64_t total);
void webd_ui_download_done(uint64_t view, uint64_t download);
void webd_ui_download_fail(uint64_t view, uint64_t download, const char* reason);

// WEBD_FEATURE_POLICY: a navigation is about to start. kind: lazyos_ui.h's LZW_NAV_*; gesture as there;
// main_frame: it replaces the page (a main-frame navigation or a new window's), not a frame of it. `request`
// is the engine's id for it (per view, never reused). A pure decision that must not block. WEBD_VERDICT_ASK:
// the engine holds the navigation (it neither loads nor refuses it) until webd_engine_navigation_answer with
// that request, or until a newer main-frame navigation or the view's end drops it (refused).
#define WEBD_VERDICT_ALLOW 0
#define WEBD_VERDICT_DENY 1
#define WEBD_VERDICT_HANDOFF 2  // not loaded: the service handed it to another program
#define WEBD_VERDICT_ASK 3
int webd_ui_navigate(uint64_t view, uint64_t request, const char* url, int kind, int gesture, int main_frame, const char* committed_url);

// WEBD_FEATURE_NOTIFICATIONS (lazyos_ui.h's lzw_ui_notify_*, per view; the notification ids are WebKit's,
// unique in the process).
void webd_ui_notify_show(uint64_t view, uint64_t id, const char* origin, const char* page_url, const char* title, const char* body, const char* tag);
void webd_ui_notify_cancel(uint64_t view, uint64_t id);
void webd_ui_notify_gone(uint64_t view, uint64_t id);
void webd_ui_notify_permission_request(uint64_t view, const char* origin);
void webd_ui_notify_permission_gone(uint64_t view, const char* origin);

// WEBD_FEATURE_BLOCKING (lazyos_ui.h's lzw_ui_blocking*, per view). webd_ui_blocking is a pure decision.
int webd_ui_blocking(uint64_t view, const char* url);
// status: lazyos_ui.h's LZW_BLOCK_*.
void webd_ui_blocking_list(uint64_t view, const char* list, int status);
void webd_ui_blocked(uint64_t view, int count);

// P4 (LazyWeb on webd): the notification decisions a page process starts with (WebKit asks its provider when it
// starts one; its record is the context's, one for every profile). Writes decision `index`'s origin, NUL-terminated,
// into `origin` (`origin_size` bytes) and returns 1 allowed, 0 blocked, -1 past the end, -2 one that does not fit
// (skipped). WebKit's main thread; a pure lookup that must not block.
int webd_ui_notify_permission_at(size_t index, char* origin, size_t origin_size);

#ifdef __cplusplus
}
#endif
