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
    long navStartMs { 0 };
    long commitMs { 0 };
    long firstPaintMs { -1 };
    int navigations { 0 };
};

extern Engine E;

// ---- log.cpp: markers and small helpers --------------------------------------------------

void say(const char* format, ...) __attribute__((format(printf, 1, 2)));
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
