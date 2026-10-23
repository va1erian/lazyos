// The engine's shared constants and the launcher hooks (docs/webd-plan.md, docs/webd.md).
//
// This header used to be the C ABI between WebKit's UI process and the one window of the
// single-page browser (`lzw_ui_*` implemented by the window, `lzw_engine_*` by the engine). That ABI
// is gone with the in-process host: LazyWeb is a client of the `webd` daemon (docs/webd-plan.md P4),
// whose C ABI over the engine's views is webkit/port/webd/webd_abi.h (one call per view). What stays
// here is what the engine's sources and webd share by name:
//
//   * LZW_FEATURE_*  the bits of a view's feature set (webd_view_config.features carries the same
//                    values, and webd's abi.cpp asserts it);
//   * LZW_NAV_*, LZW_VERDICT_*, LZW_CURSOR_*, LZW_DIALOG_*, LZW_BLOCK_*, LZW_PAINT_*  the numbers a
//                    navigation, a verdict, a pointer shape, a JavaScript dialog, a rule list and a
//                    paint plan pass through the engine and webd unchanged;
//   * the hooks WebKit's launcher (ProcessLauncherLazyOS.cpp) and the engine's children.cpp call
//                    each other through.

#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// ---- features ----

// The page's UI client: hover, pointer shape, new-window requests.
#define LZW_FEATURE_PAGE_UI 1
// JavaScript alert, confirm and prompt, answered later by the owner.
#define LZW_FEATURE_DIALOGS 2
// Attachments and responses the page cannot show become downloads.
#define LZW_FEATURE_DOWNLOADS 4
// Every navigation asks the owner.
#define LZW_FEATURE_POLICY 8
// The owner prints the markers that name pages (LOAD, TITLE, ...): this process does not.
#define LZW_FEATURE_OWN_MARKERS 16
// A website data store in the profile directory the owner names.
#define LZW_FEATURE_PROFILE 32
// The Notifications API: WebKit's provider and the page's permission requests.
#define LZW_FEATURE_NOTIFICATIONS 64
// Content blocking (issue #845): rule lists compiled into a store below the profile directory and
// attached to the page; the owner decides per main-frame navigation whether they apply. Needs
// LZW_FEATURE_PROFILE (the store lives in the profile) and a WebKit built with content extensions
// (ENABLE_CONTENT_EXTENSIONS, OptionsLazyOS.cmake).
#define LZW_FEATURE_BLOCKING 128

// ---- decisions ----

// Navigation types (WKFrameNavigationType, WebKit's own numbers).
#define LZW_NAV_LINK_CLICKED 0
#define LZW_NAV_FORM_SUBMITTED 1
#define LZW_NAV_BACK_FORWARD 2
#define LZW_NAV_RELOAD 3
#define LZW_NAV_FORM_RESUBMITTED 4
#define LZW_NAV_OTHER 5

// What a navigation's owner answers.
#define LZW_VERDICT_ALLOW 0
#define LZW_VERDICT_DENY 1
#define LZW_VERDICT_HANDOFF 2

// Pointer shapes.
#define LZW_CURSOR_DEFAULT 0
#define LZW_CURSOR_HAND 1
#define LZW_CURSOR_TEXT 2
#define LZW_CURSOR_WAIT 3
#define LZW_CURSOR_HIDDEN 4
#define LZW_CURSOR_RESIZE_H 5
#define LZW_CURSOR_RESIZE_V 6
#define LZW_CURSOR_RESIZE_NWSE 7
#define LZW_CURSOR_RESIZE_NESW 8

// What became of a rule list.
#define LZW_BLOCK_READY 0         // stored, attached to the page
#define LZW_BLOCK_NOT_STORED 1    // load: nothing stored under that id
#define LZW_BLOCK_STALE 2         // stored by an older WebKit format: compile it again
#define LZW_BLOCK_COMPILE_FAILED 3 // the JSON was refused (WebKit's parser; one bad rule fails a list)
#define LZW_BLOCK_UNAVAILABLE 4   // no store: no profile, or no content extensions in this build

// JavaScript dialogs.
#define LZW_DIALOG_ALERT 0
#define LZW_DIALOG_CONFIRM 1
#define LZW_DIALOG_PROMPT 2

// What the next paint of a view has to do (paint.cpp's plan).
#define LZW_PAINT_NOTHING 0
#define LZW_PAINT_FULL 1
#define LZW_PAINT_DELTA 2

// ---- the engine processes' launcher ----

// WebKit's launcher reports every engine process it starts, collects or kills here (a weak
// symbol there: the stand-alone programs that link WebKit without children.cpp print to stderr).
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

#ifdef __cplusplus
}
#endif
