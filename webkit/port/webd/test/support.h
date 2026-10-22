// webd-abi-test: the service's side of webd_abi.h, played by a test. The webd_ui_* callbacks (WebKit's main
// thread) record what they are told per view id under one lock; a driver thread calls webd_engine_* (any
// thread, as the Rust service does) and waits for the facts it expects. Every mode returns "" for a pass or
// the reason it failed.

#pragma once

#include <condition_variable>
#include <functional>
#include <map>
#include <mutex>
#include <stdint.h>
#include <string>
#include <vector>

#include "webd_abi.h"

namespace webdtest {

// A navigation webd_ui_navigate was asked about, and the verdict the test gave.
struct Navigation {
    uint64_t request { 0 };
    std::string url, committed;
    int kind { 0 }, gesture { 0 }, mainFrame { 0 }, verdict { 0 };
};

struct Dialog {
    uint64_t id { 0 };
    int kind { 0 };
    std::string message, defaultValue;
};

struct Download {
    std::string url, name, path, reason;
    int64_t received { 0 }, total { 0 };
    int progress { 0 };
    bool done { false }, failed { false };
};

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

    // ---- full views (support_full.cpp) ----
    int pageStates { 0 };
    std::string url, committed;
    int canBack { 0 }, canForward { 0 }, progress { 0 }, secure { 0 };
    std::vector<Navigation> navigations;
    std::vector<Dialog> dialogs;
    std::map<uint64_t, Download> downloads;
    std::vector<std::pair<std::string, int>> newWindows;
    std::vector<std::pair<std::string, int>> blockingLists;
    // Notifications: the origins that asked to notify, and what was shown (id, origin, title, body).
    std::vector<std::string> permissionRequests;
    std::vector<std::vector<std::string>> notifications;
    std::vector<uint64_t> notificationIds;
    int blocked { -1 }, blockedReports { 0 };
    // Deltas: what the test rebuilt from the posts, the posts, and the posts that no scroll call came before.
    std::vector<uint8_t> picture;
    int pictureWidth { 0 }, pictureHeight { 0 };
    int deltas { 0 }, movedDeltas { 0 }, bands { 0 }, unannounced { 0 };
    bool scrollAnnounced { false };
    int scrollX { 0 }, scrollY { 0 };
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

// The reuse pool's counts from the last INFO:POOL line the engine said (engine.cpp sayPool), or all
// -1 when it has said none: how many views adopted a pooled proxy, how many are pooled, how many are
// live, how many were retired (overflow). The cycle mode judges that the pool stays flat.
struct PoolCounts {
    long reused { -1 }, pooled { -1 }, live { -1 }, retired { -1 };
};
PoolCounts poolCounts();

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

// ---- shared with support_full.cpp: the one lock and the records ----
namespace detail {
std::mutex& lock();
std::condition_variable& changed();
// The record of an id; the lock is held.
ViewFacts& record(uint64_t id);
// Counts anything told after the promise of silence; the lock is held.
void told(ViewFacts& f);
uint64_t newId();
} // namespace detail

// ---- full views (support_full.cpp) ----

struct FullOptions {
    int features { WEBD_FEATURE_POLICY };
    std::string profile;
    int memoryOnly { 0 };
    int deltas { 0 };
    int scripts { 1 };
    int group { 0 };
    int width { 400 }, height { 300 };
    std::string userAgentSuffix;
    int noNetwork { 0 };
    int storeKind { 0 };
};

// A new id and webd_engine_view_create for a full view.
uint64_t createFull(const FullOptions& options);

// What the test answers the engine's questions; set by a mode before it acts, read on the main thread.
struct Answers {
    // webd_ui_navigate: the verdict (ALLOW unless the mode says otherwise).
    std::function<int(uint64_t view, const Navigation&)> navigate;
    // webd_ui_blocking: 1 the rule lists apply.
    std::function<int(uint64_t view, const std::string& url)> blocking;
    // webd_ui_download_start: the folder downloads go to ("" refuses them).
    std::string downloadDir;
};
void setAnswers(const Answers& answers);

// A full view's facts on one line, for a failure.
std::string describeFull(const ViewFacts& f);
// A left click at (x, y): move, press, release.
void click(uint64_t view, int x, int y);
// Waits for a full view to show a page that finished loading with this title.
bool waitPage(uint64_t view, const std::string& title, int ms = 60000);
// A page of the full modes: a title and, optionally, a link that covers the top left 200x100 pixels.
std::string linkPage(const std::string& title, const std::string& href, const std::string& more = "");

// The children's folder the host was started with (main.cpp), for the profile checks.
extern std::string childrenDirectory;
// A fresh folder below the test's temporary root.
std::string scratchFolder(const char* name);

// The modes (modes_*.cpp). "" is a pass.
std::string modeHtml();
std::string modeScript();
std::string modeConfig();
std::string modeLock();
std::string modeNetwork();
std::string modeMany();
std::string modeLost();
std::string modeCycle();
std::string modeFull();
std::string modeProfiles();
std::string modeAsk();
std::string modeDialog();
std::string modeDownload();
std::string modePopup();
std::string modeDeltas();
std::string modeBlocking();
std::string modeNotify();
std::string modeNonet();
std::string modeStore();

} // namespace webdtest
