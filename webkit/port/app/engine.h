// The UI process's internals, shared by engine.cpp (the window's requests and start-up),
// clients.cpp (what the page tells us), policy.cpp (navigation policy and downloads), uiclient.cpp
// (hover, pointer shape, new windows, JavaScript dialogs), paint.cpp (the bitmap) and log.cpp (the
// markers). The window's own ABI is lazyos_ui.h.

#pragma once

#include <WebKit/WKArray.h>
#include <WebKit/WKBase.h>
#include <WebKit/WKGeometry.h>
#include <WebKit/WKPage.h>
#include <WebKit/WKPageConfigurationRef.h>
#include <WebKit/WKView.h>

#include <atomic>
#include <functional>
#include <map>
#include <stdint.h>
#include <string>
#include <utility>
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
    // The website data store the page lives in (kept alive by the page configuration).
    WKWebsiteDataStoreRef store { nullptr };
    WKViewRef view { nullptr };
    WKPageRef page { nullptr };
    int width { kInitialWidth };
    int height { kInitialHeight };
    std::vector<uint8_t> pixels;
    bool paintScheduled { false };
    bool loading { false };
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
    // The WebProcess does not answer WebKit's responsiveness pings (the window shows its bar).
    bool unresponsive { false };
    // The window asked to end the page's process (lzw_engine_recover): the termination that follows is
    // its doing. `recoverReload`: load the page again in a fresh process, else leave it stopped.
    bool recovering { false };
    bool recoverReload { false };
    // The children's ids, kept while WebKit still says them (shutdown reaps them by id). Atomic: the
    // window's heartbeat thread reads them (lzw_engine_children).
    std::atomic<int> webPid { 0 };
    std::atomic<int> networkPid { 0 };
    // The ids as markers print them: kept after the process is reported over, which zeroes the above
    // (nothing may signal an id that may be somebody else's by now).
    int webPidShown { 0 };
    int networkPidShown { 0 };
    // The launch ordinals (children.cpp) of the processes announced above: a process with another
    // ordinal is a new one even when it took the dead one's id.
    unsigned webOrdinalShown { 0 };
    unsigned networkOrdinalShown { 0 };
    // WebKit's run loop has begun to turn (the window's heartbeat does not time a loop that has not).
    std::atomic<bool> running { false };
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
    // The context's notification manager (set with the provider, LZW_FEATURE_NOTIFICATIONS), and
    // the permission requests waiting for the window's answer, by the origin that asked
    // (retained until answered).
    WKNotificationManagerRef notifications { nullptr };
    std::vector<std::pair<std::string, WKNotificationPermissionRequestRef>> permissionRequests;
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
// answers once the processes are) and prints WEBPROCESS|NETWORKPROCESS:UP:<pid> whenever one changed.
void rememberChildren();
// The context's client: the NetworkProcess's end (after the context exists).
void installContextClient();
// The words for WebKit's termination reasons, in the markers.
const char* terminationReason(uint32_t reason);

// ---- children.cpp: the engine processes the launcher started -----------------------------

// The launch ordinal (1, 2, ...) of the `type` ("web" or "network") process `pid` the launcher has
// started and not seen end, 0 when there is none (children.cpp; any thread).
unsigned processOrdinal(const char* type, int pid);

// The engine says `type` process `pid` ended (a crash, a requested end) though the launcher has not
// collected it: the collection is retried until it has (children.cpp; any thread).
void processCrashed(const char* type, int pid);

// Ends every engine process (SIGTERM, then SIGKILL of the process group, retried for up to 10 s),
// reaps them, and prints CHILD:<name>:<pid>:GONE|ALIVE:term=..:kill=..:wait=.. for each. Returns the
// number that are left. Main thread, after the run loop.
int shutdownChildren();
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

// ---- notify.cpp: the Notifications API ------------------------------------------------------

// Registers WebKit's notification provider and tells it the decisions the window kept (a no-op
// without LZW_FEATURE_NOTIFICATIONS). After the context exists.
void installNotifications();
// The page UI client's permission request callback (uiclient.cpp installs it).
void notificationPermissionRequest(WKPageRef, WKSecurityOriginRef, WKNotificationPermissionRequestRef, const void*);
// The page that asked is going away: its pending requests are refused.
void dropNotificationRequests();

// ---- blocking.cpp: content-extension rule lists (issue #845) ----------------------------

// Before the view exists, with LZW_FEATURE_BLOCKING: the page's user content controller and the store
// below the profile. Nothing happens without the feature.
void startBlocking(WKPageConfigurationRef configuration, const std::string& profile);
// A navigation is about to begin: the rule lists go on the page or off it as lzw_ui_blocking says.
// Only a main frame decides; a frame's own navigation keeps the page's state.
void blockingNavigation(const std::string& address, bool mainFrame);
// The rule lists apply or not as the page on show says (after a navigation that did not replace it:
// a download, a failed load): the state the last decision left is put right.
void blockingSettle();
// The main frame committed (the count of blocked loads starts again) and finished loading.
void blockingCommitted();
void blockingFinished();
// The navigation client's callback for what the rule lists did (patch 0018).
void contentRuleListNotification(WKPageRef, WKURLRef, WKArrayRef, WKArrayRef, const void*);

// ---- paint.cpp ----------------------------------------------------------------------------

// All of the view is painted next (a caller that names no rectangle).
void schedulePaint();
// The view's own callback: the rectangle that changed is the one painted.
void viewNeedsDisplay(WKViewRef view, WKRect rect, const void* clientInfo);
// Input counted for the once-a-second INFO:PAINT line (a wheel turn also starts the clock the line's
// `w2d` is measured with).
void notePointer(bool wheel);
// The page's process is gone: the next paint starts from white and covers the whole view.
void invalidatePaint();

} // namespace webkitapp
