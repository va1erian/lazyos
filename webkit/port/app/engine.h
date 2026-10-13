// The UI process's internals, shared by engine.cpp (the window's requests and start-up),
// clients.cpp (what the page tells us), policy.cpp (navigation policy and downloads), uiclient.cpp
// (hover, pointer shape, new windows, JavaScript dialogs), paint.cpp (the bitmap) and log.cpp (the
// markers). The window's own ABI is lazyos_ui.h.

#pragma once

#include <WebKit/WKBase.h>
#include <WebKit/WKGeometry.h>
#include <WebKit/WKPage.h>
#include <WebKit/WKView.h>

#include <functional>
#include <map>
#include <stdint.h>
#include <string>
#include <vector>

namespace webkitapp {

constexpr int kInitialWidth = 1000;
constexpr int kInitialHeight = 500;

// A JavaScript dialog waiting for the window's answer: the retained result listener of its kind.
struct PendingDialog {
    int kind { 0 };
    WKTypeRef listener { nullptr };
};

// Everything below runs on WebKit's main thread (the run loop main() starts).
struct Engine {
    WKContextRef context { nullptr };
    // The website data store of a window with a profile directory; null: WebKit's default.
    WKWebsiteDataStoreRef store { nullptr };
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
    // The children's ids, kept while WebKit still says them (shutdown reaps them by id).
    int webPid { 0 };
    int networkPid { 0 };
    long navStartMs { 0 };
    long commitMs { 0 };
    long firstPaintMs { -1 };
    int navigations { 0 };
    // Navigations that finished loading since the start (lzw_ui_state's `finished`).
    int finished { 0 };
    // The window asked to save the next main-frame response as a download (Save Page As).
    bool saveNext { false };
    // JavaScript dialogs waiting for the window, by id.
    std::map<uint64_t, PendingDialog> dialogs;
    uint64_t nextDialog { 1 };
    // Downloads under way, by id (retained until they end).
    std::map<uint64_t, WKDownloadRef> downloads;
    uint64_t nextDownload { 1 };
    // The link last reported under the pointer.
    std::string hover;
    // Whether the page's last new-window request came with a user gesture, and its address.
    bool popupGesture { false };
    std::string popupUrl;
};

extern Engine E;

// ---- log.cpp: markers and small helpers --------------------------------------------------

// A marker that always prints: process ids, timings, failures. Nothing in it names a page. The
// window's prefix (lzw_ui_marker_prefix) and a colon come first.
void say(const char* format, ...) __attribute__((format(printf, 1, 2)));
// A marker that names a page (its URL, title or script result): printed only in an image built
// with the UI probe (LAZYOS_UI_PROBE=1), where a session reads them, and not when the window prints
// them itself (LZW_FEATURE_OWN_MARKERS). A shipped browser does not log what the user types or visits.
void sayHarness(const char* format, ...) __attribute__((format(printf, 1, 2)));
bool harness();
// Whether the window asked for a feature (lzw_ui_features, LZW_FEATURE_*): the UI process installs
// the clients for the ones it asked for and nothing else.
bool has(int feature);
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
// Reads the WebProcess's and NetworkProcess's ids from WebKit into E (they are gone from WebKit's
// answers once the processes are).
void rememberChildren();
std::string activeUrl();
std::string committedUrl();
std::string pageTitle();
// Called by the clients when a main-frame navigation starts: the pending dialogs of the page that
// is going away are answered "no" and forgotten (their listeners die with the page).
void dropDialogs();

// ---- policy.cpp: navigation policy and downloads -----------------------------------------

void decidePolicyForNavigationAction(WKPageRef, WKNavigationActionRef, WKFramePolicyListenerRef, WKTypeRef, const void*);
void decidePolicyForNavigationResponse(WKPageRef, WKNavigationResponseRef, WKFramePolicyListenerRef, WKTypeRef, const void*);
void navigationActionDidBecomeDownload(WKPageRef, WKNavigationActionRef, WKDownloadRef, const void*);
void navigationResponseDidBecomeDownload(WKPageRef, WKNavigationResponseRef, WKDownloadRef, const void*);

// ---- uiclient.cpp: the page's UI client and the view's pointer ---------------------------

void installUiClient();
// The view client's pointer-shape callback (engine.cpp installs it with the others).
void viewSetCursor(WKViewRef view, uint32_t cursorType, const void* clientInfo);

// ---- paint.cpp ----------------------------------------------------------------------------

void schedulePaint();
void viewNeedsDisplay(WKViewRef view, WKRect rect, const void* clientInfo);

} // namespace webkitapp
