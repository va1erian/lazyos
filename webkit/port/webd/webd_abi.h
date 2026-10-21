// The C ABI between webd's WebKit half (C++, webkit/port/webd/abi.cpp over the engine of webkit/port/app:
// one Host, any number of Views) and webd's service half (Rust, xui-app/crates/webd). docs/webd-plan.md
// sections 3.2 and 3.6, phase P2.
//
// This is the contract of P2 slice 1 (LIGHT views only). Both halves are written against this file; change
// it only together with both. Fields and calls a light view does not use are absent on purpose: the full
// interface (navigation, dialogs, downloads, notifications, blocking, profiles) appends to this file in a
// later slice, so every struct starts with its size and every enum value is append-only.
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
// Pixels. Every frame is opaque RGBA, 4 bytes per pixel, top row first, stride width * 4. Slice 1 sends WHOLE
// frames only (no deltas, bands or scroll shifts): the engine paints a view's whole area whenever anything in
// it changed, at most once per paint interval. Damage tracking arrives with the full interface.

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
    // policy. Slice 1 accepts only light = 1; webd_engine_view_create ignores a view with light = 0.
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
} webd_view_config;

// ---- the service asks the engine (any thread) ----

// Creates the WebKit Host (the context and its engine processes' launcher). Call once, on the process's main
// thread, before the run loop turns; returns 0 on success.
int webd_engine_start(void);

// Creates a view with this id on the run loop. Asynchronous: success is the first webd_ui_state for the id.
// A refused configuration (not light, bad size) is answered by webd_ui_view_lost(id, WEBD_LOST_REFUSED).
void webd_engine_view_create(uint64_t view, const webd_view_config* config);

// Ends a view and its page. No webd_ui_* call about the id follows once the engine has run this.
void webd_engine_view_destroy(uint64_t view);

// Loads the caller's HTML (`html`, `len` bytes of UTF-8, copied before the call returns) with this base URL
// ("about:blank" or an opaque origin; anything with a network or file scheme is refused: nothing loads and the
// next webd_ui_state carries a non-empty error). Replaces the page.
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

#ifdef __cplusplus
}
#endif
