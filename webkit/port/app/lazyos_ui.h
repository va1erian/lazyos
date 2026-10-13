// The C ABI between WebKit's UI process (engine.cpp, main.cpp) and the xui window
// (xui-app/crates/webkit/src/ffi.rs, a Rust static library): docs/webkit-port-plan.md, W5.
//
// Two threads. WebKit's run loop owns the process's main thread; main() starts a
// second one that calls lzw_ui_main() and stays in it until the window closes.
// Everything the window asks of WebKit (lzw_engine_*, implemented in engine.cpp)
// only queues work for the run loop and returns, so it may be called from the
// window's thread at any rate. Everything WebKit says (lzw_ui_frame, lzw_ui_state,
// implemented in Rust) is safe to call from the run loop's thread.

#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// ---- implemented by the window (Rust) ----

// The calling user's user-memory charge and limit in bytes, as the kernel counts them (the length
// of every mapping, not the pages touched). Returns 0 on success.
int lzw_ui_user_memory(uint64_t* used, uint64_t* limit);

// Runs the window until it is closed; returns the process exit status. argv[1], if
// it is not an option, is the first address to open.
int lzw_ui_main(int argc, const char* const* argv);

// WebKit painted a frame: width * height * 4 bytes of opaque RGBA, top row first.
// The bytes are copied before the call returns.
void lzw_ui_frame(const uint8_t* rgba, int width, int height);

// The page's state changed (UTF-8, never null). progress is in thousandths.
void lzw_ui_state(const char* url, const char* title, int loading, int can_back, int can_forward, int progress);

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

// A wheel step at (x, y). ticks_y > 0 scrolls up, ticks_x > 0 scrolls right.
void lzw_engine_wheel(int x, int y, int ticks_x, int ticks_y, uint32_t mods);

// A key press or release. keysym is an X11 key symbol (libwpe's key codes); text is what
// the key types ("" for a key that types nothing).
void lzw_engine_key(int press, uint32_t keysym, const char* text, uint32_t mods);

void lzw_engine_focus(int focused);

// Creates the context, the page and the view; must run on the main thread, before
// WKRunLoopRunMain(). Returns 0 on success.
int lzw_engine_start(void);

// Ends WKRunLoopRunMain() (callable from any thread).
void lzw_engine_quit(void);

#ifdef __cplusplus
}
#endif
