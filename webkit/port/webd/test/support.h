// webd-abi-test: the service's side of webd_abi.h, played by a test. The webd_ui_* callbacks (WebKit's main
// thread) record what they are told per view id under one lock; a driver thread calls webd_engine_* (any
// thread, as the Rust service does) and waits for the facts it expects. Every mode returns "" for a pass or
// the reason it failed.

#pragma once

#include <functional>
#include <stdint.h>
#include <string>
#include <vector>

#include "webd_abi.h"

namespace webdtest {

// What the service has been told about one view.
struct ViewFacts {
    std::string title, error;
    int loading { 0 }, finished { 0 }, states { 0 };
    int frames { 0 }, width { 0 }, height { 0 };
    // The dominant colour of the last frame ('r', 'g', 'b', 'w', '?') and of every frame, in order.
    char last { '?' };
    std::string seen;
    int scrolls { 0 };
    std::vector<std::string> links, clipboard;
    bool lost { false };
    int lostReason { 0 };
    // The engine promised silence once it had run view_destroy / reported the loss: `silent` is set by the
    // fence task that runs behind the destroy, and anything told afterwards is a violation.
    bool silent { false };
    int violations { 0 };
};

// The facts of an id (made when first named). Callers hold no lock; this copies.
ViewFacts facts(uint64_t id);

// Waits until `ready` is true (evaluated under the lock, on every change) or `ms` pass.
bool waitFor(const std::function<bool()>& ready, int ms);
bool waitView(uint64_t id, const std::function<bool(const ViewFacts&)>& ready, int ms);

// Engine processes the launcher announced, from the markers the engine says.
struct Processes {
    int webLaunches { 0 }, networkLaunches { 0 }, webExits { 0 }, networkExits { 0 };
};
Processes processes();
// Waits until `ready` is true of the counts (or `ms` pass).
bool waitProcesses(const std::function<bool(const Processes&)>& ready, int ms);
// Whether the engine said a line that starts with `prefix`.
bool said(const std::string& prefix);

struct Options {
    int scripts { 0 };
    int group { 0 };
    int width { 240 }, height { 160 };
    int colorScheme { 0 };
    int fontSize { 0 };
    const char* style { nullptr };
};

// A new id (never reused), its record, and webd_engine_view_create.
uint64_t create(const Options& options);
void loadHtml(uint64_t id, const std::string& html, const char* base = "about:blank");
// view_destroy, then waits for the engine to have run it: what is told about the id after that counts.
void destroy(uint64_t id);

// "<!doctype html><title>t</title><body style=margin:0;background:c>" and `body` after it.
std::string page(const char* title, const char* colour, const std::string& body = "");
// A view painted `colour` ('r', 'g', 'b') with this title and has finished `finished` loads.
bool shows(const ViewFacts& f, const char* title, char colour, int finished);

// The dominant colour of an RGBA frame: 'r', 'g', 'b', 'w' (white), '?'.
char dominant(const uint8_t* rgba, int width, int height);

long monotonicMs();
// One line of the run, after "WEBD:".
void note(const char* format, ...) __attribute__((format(printf, 1, 2)));

// The modes (modes_*.cpp). "" is a pass.
std::string modeHtml();
std::string modeScript();
std::string modeConfig();
std::string modeLock();
std::string modeNetwork();
std::string modeMany();
std::string modeLost();
std::string modeCycle();

} // namespace webdtest
