// The UI process's internals, shared by engine.cpp (the window's requests and start-up),
// clients.cpp (what the page tells us), paint.cpp (the bitmap) and log.cpp (the markers).
// The window's own ABI is lazyos_ui.h.

#pragma once

#include <WebKit/WKBase.h>
#include <WebKit/WKGeometry.h>
#include <WebKit/WKPage.h>
#include <WebKit/WKView.h>

#include <functional>
#include <stdint.h>
#include <string>
#include <vector>

namespace webkitapp {

constexpr int kInitialWidth = 1000;
constexpr int kInitialHeight = 500;

// Everything below runs on WebKit's main thread (the run loop main() starts).
struct Engine {
    WKContextRef context { nullptr };
    WKViewRef view { nullptr };
    WKPageRef page { nullptr };
    int width { kInitialWidth };
    int height { kInitialHeight };
    std::vector<uint8_t> pixels;
    bool paintScheduled { false };
    bool loading { false };
    bool processesReported { false };
    bool formNavigation { false };
    bool inputPending { false };
    bool committedSincePaint { false };
    bool loadFinished { false };
    std::string lastTitleSaid;
    // What the status line says about a failed load; empty when the last one worked.
    std::string error;
    // The WebProcess is gone (crashed, or did not start): the page is blank until the next load.
    bool crashed { false };
    // The user pressed Stop: the failure WebKit reports for it is not one.
    bool stopRequested { false };
    long navStartMs { 0 };
    long commitMs { 0 };
    long firstPaintMs { -1 };
    int navigations { 0 };
};

extern Engine E;

// ---- log.cpp: markers and small helpers --------------------------------------------------

// A marker that always prints: process ids, timings, failures. Nothing in it names a page.
void say(const char* format, ...) __attribute__((format(printf, 1, 2)));
// A marker that names a page (its URL, title or script result): printed only in an image built
// with the UI probe (LAZYOS_UI_PROBE=1), where a session reads them. A shipped browser does not
// log what the user types or visits.
void sayHarness(const char* format, ...) __attribute__((format(printf, 1, 2)));
bool harness();
long elapsedMs();
std::string utf8(WKStringRef string);
std::string utf8(WKURLRef url);
// A line of text for a marker: no newlines, so one marker stays one line.
std::string oneLine(std::string text);

// ---- engine.cpp ---------------------------------------------------------------------------

// Queues a task on the run loop (callable from any thread).
void onMain(std::function<void()> task);

// ---- clients.cpp: the page's news ---------------------------------------------------------

void installClients();
void postState();
std::string activeUrl();
std::string pageTitle();

// ---- paint.cpp ----------------------------------------------------------------------------

void schedulePaint();
void viewNeedsDisplay(WKViewRef view, WKRect rect, const void* clientInfo);

} // namespace webkitapp
