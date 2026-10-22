// The C ABI of webd's host (xui-app/webd, the Rust static library `libxui_webd_host.a`) as main.cpp calls it.
// The service's own contract with the engine is webd_abi.h; this is only how the program starts and stops it.
//
// Threads. The process's FIRST thread runs the Rust service (webd_host_main): `init` and the shell know an
// app by its first thread, so the registry name and the lifecycle line live there. WebKit's run loop runs on
// a thread main.cpp makes, and calls webd_engine_* (webd_abi.h) from nowhere else than the service thread.

#pragma once

#ifdef __cplusplus
extern "C" {
#endif

// Makes the doorbell and the engine's event queue and installs it as the sink of the webd_ui_* callbacks, and
// points the engine at its rule-store folder (LAZYOS_WEBD_RULES_DIR). Once, on the first thread, before
// webd_engine_start. 0 on success.
int webd_host_init(void);

// The profiles' root ($HOME/.apps/os.lazy.webd/profiles), made by webd_host_init, where the engine's
// processes start (webd_engine_start_host); NULL when there is none (light views only). Valid for the
// process's life.
const char* webd_host_children_directory(void);

// Serves app.os.lazy.webd.light and app.os.lazy.webd.view until `init` says Quit, SIGTERM (webd_host_signal_quit) or the engine's
// main thread stops answering. Blocks. Returns the exit status: 0 clean, 1 it could not serve, 4 hung.
int webd_host_main(void);

// Asks webd_host_main to return. Any thread, and a signal handler (an atomic store and a write).
void webd_host_signal_quit(void);

// The WebKit host did not start (`why`, NUL-terminated).
void webd_host_fail(const char* why);

// The last marker: `status` of webd_host_main and the engine processes `surviving` after the shutdown
// (-1: not known, the engine hung).
void webd_host_report(int status, int surviving);

#ifdef __cplusplus
}
#endif
