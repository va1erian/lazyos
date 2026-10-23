// The UI process's internals, shared by engine.cpp (the host and its views), clients.cpp (what a page
// tells us), policy.cpp (navigation policy and downloads), uiclient.cpp (hover, pointer shape, new
// windows, JavaScript dialogs), paint.cpp (the bitmap), notify.cpp, blocking.cpp, children.cpp (the
// engine processes) and log.cpp (the markers).
//
// One Host per process (one WKContext, the engine processes, the notification manager) and any number
// of Views (a page each, with its own state). Everything a view says goes to its ViewSink, everything
// the host needs of its owner is a HostEnv: neither names a C ABI. The owners are the webd daemon
// of docs/webd-plan.md (webkit/port/webd/abi*.cpp, sink_full.cpp) and the two-view test (views/main.cpp), which
// implement the same two interfaces.

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
#include <memory>
#include <stdint.h>
#include <string>
#include <utility>
#include <vector>

namespace webkitapp {

constexpr int kInitialWidth = 1000;
constexpr int kInitialHeight = 500;

// A JavaScript dialog waiting for the owner's answer: the retained result listener of its kind.
struct PendingDialog {
    int kind { 0 };
    WKTypeRef listener { nullptr };
};

// What a page reports about itself (ViewSink::state). UTF-8, never null.
struct ViewState {
    const char* url { "" };
    const char* title { "" };
    bool loading { false };
    bool canBack { false };
    bool canForward { false };
    int progress { 0 }; // thousandths
    const char* error { "" };
    bool crashed { false };
    const char* committedUrl { "" };
    bool secure { false };
    int finished { 0 };
    // Why the page's process ended when `crashed`: "crash", "memory", "cpu" or "requested" (terminationReason).
    const char* terminated { "" };
    // Bumped by a state the owner must hear even when nothing else in it changed (a refused load).
    int serial { 0 };
};

// What a view tells its owner. Called on WebKit's main thread (the run loop's); none may block.
// The defaults refuse or ignore, so an owner implements only what its views use (ViewConfig::features).
class ViewSink {
public:
    virtual ~ViewSink() = default;

    virtual void state(const ViewState&) { }
    virtual void responsive(bool) { }
    // A light view (ViewConfig::light) refused a navigation of the main frame (a link click, a form submit, a
    // meta refresh, a script's location change) and loaded nothing: the address it asked for, absolute.
    virtual void linkClicked(const char* /*url*/) { }

    // Painting. The damage a paint has to cover is kept by the owner (paint*), in the form it likes;
    // the paint asks for the plan (kPaintNothing, kPaintFull, kPaintDelta) and sends pixels back.
    virtual void paintResize(int /*width*/, int /*height*/) { }
    virtual void paintFull() { }
    virtual void paintRect(int /*x*/, int /*y*/, int /*width*/, int /*height*/) { }
    virtual void paintUpdate(int /*sx*/, int /*sy*/, int /*sw*/, int /*sh*/, int /*dx*/, int /*dy*/, const int* /*rects*/, int /*count*/) { }
    static constexpr int kPaintNothing = 0;
    static constexpr int kPaintFull = 1;
    static constexpr int kPaintDelta = 2;
    // Without an owner that tracks damage every paint is a whole one.
    virtual int paintPlan(int* /*scroll*/, int* /*rects*/, int /*max*/, int* count) { *count = 0; return kPaintFull; }
    // Opaque RGBA, top row first, copied before the call returns.
    virtual void frame(const uint8_t* /*rgba*/, int /*width*/, int /*height*/) { }
    // Returns non-zero when the owner cannot take the delta (the next frame is a whole one).
    virtual int frameDelta(const uint8_t* /*rgba*/, int /*width*/, int /*height*/, const int* /*scroll*/, const int* /*rects*/, int /*count*/) { return 1; }
    virtual void band(int /*docX*/, int /*docY*/, int /*width*/, int /*height*/, const uint8_t* /*rgba*/) { }
    virtual void scrollOffset(int /*x*/, int /*y*/, bool /*atBottom*/) { }

    // The page UI client (ViewConfig::features & LZW_FEATURE_PAGE_UI).
    virtual void hover(const char* /*url*/) { }
    virtual void cursor(int /*shape*/) { }
    virtual void newWindow(const char* /*url*/, bool /*gesture*/) { }
    // LZW_FEATURE_DIALOGS: returns at once; the answer is View::replyDialog.
    virtual void dialog(uint64_t /*id*/, int /*kind*/, const char* /*message*/, const char* /*defaultValue*/) { }
    // LZW_FEATURE_DOWNLOADS: the owner picks the file (path, NUL-terminated within pathSize); non-zero refuses.
    virtual int downloadStart(uint64_t /*id*/, const char* /*url*/, const char* /*name*/, int64_t /*total*/, char* /*path*/, size_t /*pathSize*/) { return 1; }
    virtual void downloadProgress(uint64_t /*id*/, int64_t /*received*/, int64_t /*total*/) { }
    virtual void downloadDone(uint64_t /*id*/) { }
    virtual void downloadFail(uint64_t /*id*/, const char* /*reason*/) { }
    // LZW_FEATURE_POLICY: LZW_VERDICT_*; a pure decision.
    virtual int navigate(const char* /*url*/, int /*kind*/, bool /*gesture*/, const char* /*committed*/) { return 0; }
    // The same question with the view's id for it (`request`, per view, never reused) and whether it replaces the
    // page (`mainFrame`: the main frame's navigation or a new window's). It may also answer kVerdictAsk: the
    // navigation is then held (View::held) until View::answerNavigation, a newer main-frame navigation or the
    // view's end. The default asks `navigate`, so an owner of that question holds nothing.
    // (kVerdictDeny is LZW_VERDICT_DENY; policy.cpp asserts it.)
    static constexpr int kVerdictDeny = 1;
    static constexpr int kVerdictAsk = 3;
    virtual int navigateRequest(uint64_t /*request*/, const char* url, int kind, bool gesture, bool /*mainFrame*/, const char* committed)
    {
        // An owner of the old question never holds: an answer it did not have before is a refusal, as it was.
        const int verdict = navigate(url, kind, gesture, committed);
        return verdict == kVerdictAsk ? kVerdictDeny : verdict;
    }

    // LZW_FEATURE_NOTIFICATIONS.
    virtual void notifyShow(uint64_t /*id*/, const char* /*origin*/, const char* /*pageUrl*/, const char* /*title*/, const char* /*body*/, const char* /*tag*/) { }
    virtual void notifyCancel(uint64_t /*id*/) { }
    virtual void notifyGone(uint64_t /*id*/) { }
    virtual void notifyPermissionRequest(const char* /*origin*/) { }
    virtual void notifyPermissionGone(const char* /*origin*/) { }

    // LZW_FEATURE_BLOCKING: whether the rule lists apply to a page at `url`; what became of a list;
    // how many loads were blocked on the page since it committed.
    virtual int blocking(const char* /*url*/) { return 1; }
    virtual void blockingList(const char* /*id*/, int /*status*/) { }
    virtual void blocked(int /*count*/) { }
};

// What the host needs of whoever owns the process.
class HostEnv {
public:
    virtual ~HostEnv() = default;
    // The LZW_FEATURE_* bits that decide how the host marks its output (LZW_FEATURE_OWN_MARKERS).
    virtual int features() = 0;
    // One marker line, without prefix or newline, written whole and from any thread.
    virtual void say(const char* line) = 0;
    // Whether this image carries the UI probe (markers that name pages).
    virtual bool probe() = 0;
    // The calling user's memory charge and limit in bytes; 0 on success.
    virtual int userMemory(uint64_t* used, uint64_t* limit) = 0;
    // The decisions about which sites may notify, read when WebKit starts a page's process: -1 past
    // the end, -2 a row that does not fit, else 1 allowed, 0 blocked.
    virtual int notifyPermissionAt(size_t /*index*/, char* /*origin*/, size_t /*size*/) { return -1; }
};

// How a view is made.
struct ViewConfig {
    // The LZW_FEATURE_* bits this view uses: what is installed for it and nothing else (a view that
    // asks for none gets the page, its state, its input and its pixels).
    int features { 0 };
    // The directory for cookies and site data, or empty for a non-persistent store of its own.
    std::string profile;
    // The page's data stays in memory (issue #940): the store is non-persistent, the directory (if any) holds
    // only what the engine keeps there for itself (the content-blocking store, the children's folder).
    bool memoryOnly { false };
    // Views of one owner with the same non-zero group and the same profile directory (or both none) share
    // one WebProcess and one data store: a page costs its own memory, not a 107 MiB process. A view with
    // no group, or whose profile differs from its group's first view, gets a process of its own. The
    // pages of a group can see each other's storage and script each other by name (they are related
    // pages), so a group is one owner's, never a mix of untrusting parties. The engine cannot tell owners
    // apart (a sink is per view): the owner of several clients (webd) derives the number from the
    // kernel-stamped client and the profile, never from anything a client sent.
    int group { 0 };
    int width { kInitialWidth };
    int height { kInitialHeight };
    // A LIGHT view (docs/webd-plan.md 3.6; light.cpp): content comes from View::loadHtml only. Everything
    // below is read only when `light` is set, so a full view is as it always was. A light view
    // has a data store of its own that keeps nothing (give it no profile), scripts as `scripts` says, no
    // network of any kind (a content rule list that blocks every load but data:, blob: and about: URLs, and a
    // navigation policy that loads nothing the owner did not ask for), and the owner's features should be 0.
    bool light { false };
    bool scripts { false };
    // 1: prefers-color-scheme is dark. 0: follows the page (light).
    int colorScheme { 0 };
    // The page's default and minimum font size in CSS pixels; 0 keeps WebKit's.
    int defaultFontSize { 0 };
    // Extra CSS for every page loaded with loadHtml (empty: none).
    std::string userStyle;
    // A full (not light) view whose owner set `scripts`, `defaultFontSize`, `colorScheme` and `userStyle` for it
    // too (webd, fullview.cpp). Unset (LazyWeb+), a browser view is as it always was.
    bool configured { false };
    // Appended to the user agent (fullview.cpp); empty: WebKit's own.
    std::string userAgentSuffix;
    // Replaces the user agent (fullview.cpp); empty: WebKit's own, with the suffix if there is one.
    std::string userAgent;
    // A configured full view with the light views' network lock on its controller (no load but data:, blob:,
    // about:). It is made only when the lock is ready, and never with LZW_FEATURE_BLOCKING (whose rebuilds of
    // the controller's lists would drop it).
    bool noNetwork { false };
    // Which kind of store the owner keeps this view's data in (webd: WEBD_STORE_OWN / WEBD_STORE_PRIVATE). Views
    // join a group mate only with an equal value, so two kinds of in-memory store of one group stay apart.
    int storeKind { 0 };
};

// A navigation the owner was asked about and has not answered (ViewSink::kVerdictAsk): its retained listener
// and what finishing it needs.
struct HeldNavigation {
    WKFramePolicyListenerRef listener { nullptr };
    std::string address;
    bool mainFrame { false };
    bool download { false };
};

struct Host;
struct PaintState;
struct Profile;

// One page. Everything below runs on WebKit's main thread (the run loop main() starts).
struct View {
    View(int id, const ViewConfig& config, ViewSink* sink);
    ~View();
    View(const View&) = delete;
    View& operator=(const View&) = delete;

    const int id;
    const ViewConfig config;
    ViewSink* const sink;
    bool has(int feature) const { return (config.features & feature) != 0; }

    // The website data store's holder (shared by the views of one profile directory).
    std::shared_ptr<Profile> profile;
    WKViewRef view { nullptr };
    WKPageRef page { nullptr };
    int width { kInitialWidth };
    int height { kInitialHeight };
    std::vector<uint8_t> pixels;
    std::unique_ptr<PaintState> painting;
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
    // This page has only ever been the single page in its WebProcess (never a related page of a group).
    // Only such a proxy may be pooled and reused: reusing a proxy that once shared a process aborts the
    // UI process on this port (webkit-views group, the family of patches 0021/0027). Set false when the
    // view is made as, or becomes, a group mate (engine.cpp). See destroyView / createView.
    bool soleEver { true };
    // The owner asked this page to load something (load, loadHtml, reload): from then on a process may be
    // launching for it even while WebKit has no id for it yet. destroyView ends such a process.
    bool loadRequested { false };
    // The user pressed Stop: the failure WebKit reports for it is not one.
    bool stopRequested { false };
    // The WebProcess does not answer WebKit's responsiveness pings.
    bool unresponsive { false };
    // The owner asked to end the page's process (recover): the termination that follows is its doing.
    // `recoverReload`: load the page again in a fresh process, else leave it stopped.
    bool recovering { false };
    bool recoverReload { false };
    // The ids of this page's process as the last announcement said them (markers print them after
    // the process is reported over, which zeroes the host's), and the launch ordinals
    // (children.cpp) of the processes announced: a process with another ordinal is a new one even
    // when it took the dead one's id.
    int webPidShown { 0 };
    int networkPidShown { 0 };
    unsigned webOrdinalShown { 0 };
    unsigned networkOrdinalShown { 0 };
    bool webProcessSeen { false };
    long navStartMs { 0 };
    long commitMs { 0 };
    long firstPaintMs { -1 };
    int navigations { 0 };
    // Navigations that finished loading since the start (ViewState::finished).
    int finished { 0 };
    // The owner asked to save the next main-frame response as a download (Save Page As).
    bool saveNext { false };
    // JavaScript dialogs waiting for the owner, by id.
    std::map<uint64_t, PendingDialog> dialogs;
    uint64_t nextDialog { 1 };
    // Downloads under way, by id (retained until they end).
    std::map<uint64_t, WKDownloadRef> downloads;
    uint64_t nextDownload { 1 };
    // Permission requests waiting for the owner's answer, by the origin that asked (retained).
    std::vector<std::pair<std::string, WKNotificationPermissionRequestRef>> permissionRequests;
    // Navigations held for the owner's answer, by request id, and the next id (ViewSink::navigateRequest).
    std::map<uint64_t, HeldNavigation> held;
    uint64_t nextRequest { 1 };
    // The link last reported under the pointer.
    std::string hover;
    // Whether the page's last new-window request came with a user gesture, and its address.
    bool popupGesture { false };
    std::string popupUrl;
    // Content blocking: the page's user content controller and what the last navigation decided.
    WKUserContentControllerRef controller { nullptr };
    bool blockWanted { true };
    bool blockAttached { false };
    int blockedCount { 0 };
    // Light views (light.cpp): the loadHtml loads whose navigation the policy has not yet allowed, and the
    // address they are for (the base URL as WebKit spells it, without a fragment). Why the page's process
    // ended, for ViewState::terminated.
    int lightLoads { 0 };
    std::string lightBase;
    std::string terminated;
    int stateSerial { 0 };

    // The owner's calls, by name (the C ABI's lzw_engine_* for view 1). Main thread only.
    // A light view's only load: the owner's HTML (UTF-8) with a base URL that is about:blank or a data: URL
    // (empty is about:blank). Anything else loads nothing and the next state carries an error. Replaces the page.
    void loadHtml(const std::string& html, const std::string& baseUrl);
    void load(const std::string& url);
    void reload();
    void stop();
    void back();
    void forward();
    void resize(int width, int height);
    void mouse(int kind, int button, int x, int y, uint32_t mods);
    void wheel(int x, int y, int ticksX, int ticksY, uint32_t mods);
    void key(bool press, uint32_t keysym, const std::string& text, uint32_t mods);
    void focus(bool focused);
    void download(const std::string& url);
    void downloadCancel(uint64_t id);
    void replyDialog(uint64_t id, bool accepted, const std::string& value);
    // Finishes held navigation `id` as the normal path would (allow) or refuses it; an id no longer held is ignored.
    void answerNavigation(uint64_t id, bool allow);
    void recover(bool reload);
    // Ends the process other views share and marks every view in it as stopped on request (no reload): the owner
    // makes its views again.
    void recoverGroup();
};

// The data store a profile directory names, with the rule lists compiled into it.
struct Profile {
    std::string directory; // empty: ephemeral
    WKWebsiteDataStoreRef store { nullptr };
    WKUserContentExtensionStoreRef rules { nullptr };
    // The compiled lists that are ready, by id: each holds the reference the store's callback gave.
    std::map<std::string, WKTypeRef> lists;
    // Bumped when a list is forgotten, so a compile or lookup still in flight for it is dropped.
    std::map<std::string, int> generation;
    ~Profile();
};

// A page proxy a destroyed view left behind, kept so a later view of the same owner and class can
// reuse it instead of making a new one: a WebPageProxy cannot be released on this port (destroyView),
// so without this a create/destroy loop grows the process without bound (docs/webd-plan.md, "What
// still grows without bound", P5). The WKView holds the page; this struct owns the WKView reference
// (released only at process exit, never) and the class key a new view must match to adopt it.
struct ParkedPage {
    WKViewRef view { nullptr };
    WKPageRef page { nullptr };
    std::shared_ptr<Profile> profile;
    ViewConfig config;
};

// Everything that is one per process.
struct Host {
    HostEnv* env { nullptr };
    WKContextRef context { nullptr };
    std::map<int, std::unique_ptr<View>> views;
    // Destroyed views whose proxy cannot be pooled, kept forever (a WebPageProxy cannot be released): every
    // full view, a view whose process a sibling still used (the page was unloaded to about:blank), group 0,
    // and a view destroyed while its process was not known to be over. It grows with each such destroy,
    // a full view's create/destroy loop included; only light views are reused (destroyView, poolable).
    std::vector<std::unique_ptr<View>> parked;
    // Reusable page proxies of destroyed LIGHT views that were the only page of their process and whose
    // process is over: the same client's next light view of the same class adopts one rather than making
    // a new proxy (engine.cpp, poolable / parkForReuse / takeFromPool). Bounded by kPoolMax; an overflow
    // proxy, which still cannot be released, goes to `retired`.
    std::vector<ParkedPage> pool;
    // Pooled proxies evicted when the pool was full: kept forever like `parked` (whole, so the view's
    // reference and its data store both stay alive), only so they are not released (releasing a
    // WebPageProxy aborts the UI process). Never adopted again.
    std::vector<ParkedPage> retired;
    // How many views have adopted a pooled proxy (INFO:POOL, the cycle test).
    uint64_t poolReused { 0 };
    int nextView { 1 };
    // The persistent profiles in use, by directory (an ephemeral store is the view's alone).
    std::map<std::string, std::weak_ptr<Profile>> profiles;
    // ... which are never torn down (engine.cpp, profileFor).
    std::vector<std::shared_ptr<Profile>> keptProfiles;
    // The children's environment was decided once at start (startHostWithChildren): no profile changes it.
    bool environmentFixed { false };
    // The context's notification manager (set with the provider, LZW_FEATURE_NOTIFICATIONS), and
    // which view each live notification belongs to.
    WKNotificationManagerRef notifications { nullptr };
    std::map<uint64_t, int> notificationViews;
    // The children's ids, kept while WebKit still says them (shutdown reaps them by id). Atomic: the
    // window's heartbeat thread reads them (lzw_engine_children).
    std::atomic<int> webPid { 0 };
    std::atomic<int> networkPid { 0 };
    // WebKit's run loop has begun to turn (the window's heartbeat does not time a loop that has not).
    std::atomic<bool> running { false };
};

extern Host H;

// The view with this id, or null (it was destroyed). Main thread.
View* viewById(int id);
// The view whose page this is, or null.
View* viewOfPage(WKPageRef page);
// The view a callback's client info names (installed by createView).
inline View* viewOf(const void* clientInfo) { return static_cast<View*>(const_cast<void*>(clientInfo)); }

// ---- engine.cpp: the host and its views ----------------------------------------------------------

// Creates the WKContext and the notification provider. Before the run loop turns; main thread.
// `profileHint` is the directory of the profile the first view will use, if the owner knows it: the
// children take their environment when they start, so it is set before anything can start one.
bool startHost(HostEnv* env, const std::string& profileHint = std::string());
// startHost for an owner of several profiles (webd): the children's environment is decided here, once, and no
// view changes it (profile.cpp, fixChildrenEnvironment). False when `childrenDirectory` cannot be made.
bool startHostWithChildren(HostEnv* env, const std::string& childrenDirectory);
// A page with its own state; null when WebKit refuses. Main thread.
View* createView(const ViewConfig& config, ViewSink* sink);
// Ends a page: its clients are cleared, its dialogs and downloads answered, its paint state and bitmaps
// freed, its WebProcess ended unless another view uses it (then the page is unloaded: about:blank), and the view is gone for the owner (its id
// names nothing). The page proxy itself is parked, never released: WebKit's WebPageProxy aborts the UI
// process when it is destroyed on this port. Not from inside a call of the view's own sink (the code that
// called it carries on and would start a paint thread on the parked view): queue it with onMain. What stays per view that ever was: the proxy and, for a view
// with a store of its own, the store and its session. Main thread.
void destroyView(int id);
// Queues a task on the run loop (callable from any thread).
void onMain(std::function<void()> task);
// Queues a task for a view, dropped if the view is gone when the run loop gets to it.
void onView(int id, std::function<void(View&)> task);

// ---- profile.cpp: data stores and the children's environment ------------------------------------

// `mkdir -p`, private to the user (0700).
bool makePrivateDirectories(const std::string& path);
// What the children inherit for a host that has one profile (LazyWeb+): the first persistent profile's directory
// and cookie jar. Nothing once the environment is fixed.
void exportProfileEnvironment(const std::string& profile);
// The children start in `directory` (created) and curl's default cookie database is in memory, for every profile
// to come: H.environmentFixed. Before anything can start a process.
bool fixChildrenEnvironment(const std::string& directory);
// The profile a view of this configuration lives in: the directory's, when another view has it open already,
// else a new one.
std::shared_ptr<Profile> profileFor(const std::string& directory, bool memoryOnly);

// ---- fullview.cpp: a full view's configuration (webd) ---------------------------------------------

// Before the view exists, for a view with ViewConfig::configured: scripts, font size, user style, the network
// lock. False when the view must not be made (the lock is not ready, or it would be dropped).
bool configureFull(View& view, WKPageConfigurationRef configuration);
// After the page exists: the user agent the view was made with (an agent, or WebKit's with a suffix), when it has one,
// or `override` (webd_engine_set_user_agent) in its place; an empty `override` is the made-with agent again.
void applyUserAgent(View& view, const std::string& override = std::string());
// A full view's load of the owner's HTML (UTF-8) with a base URL of the about:, data:, http: or https: scheme
// (empty is about:blank); any other loads nothing and the next state carries an error. Replaces the page.
void loadFullHtml(View& view, const std::string& html, const std::string& baseUrl);

// ---- log.cpp: markers and small helpers ----------------------------------------------------------

// A marker that always prints: process ids, timings, failures. Nothing in it names a page. The
// owner's prefix and a colon come first.
void say(const char* format, ...) __attribute__((format(printf, 1, 2)));
// A marker that names a page (its URL, title or script result): printed only in an image built
// with the UI probe (LAZYOS_UI_PROBE=1), where a session reads them, and not when the window prints
// them itself (LZW_FEATURE_OWN_MARKERS). A shipped browser does not log what the user types or visits.
void sayHarness(const char* format, ...) __attribute__((format(printf, 1, 2)));
bool harness();
// Whether the owner asked for a host-wide feature (HostEnv::features).
bool has(int feature);
long elapsedMs();
std::string utf8(WKStringRef string);
std::string utf8(WKURLRef url);
// A line of text for a marker: no newlines, so one marker stays one line.
std::string oneLine(std::string text);

// ---- clients.cpp: the page's news ----------------------------------------------------------------

void installClients(View& view);
void postState(View& view);
// Reads the WebProcess's and NetworkProcess's ids from WebKit into the host and the view (they are gone
// from WebKit's answers once the processes are) and prints WEBPROCESS|NETWORKPROCESS:UP:<pid> whenever
// one changed.
void rememberChildren(View& view);
// The context's client: the NetworkProcess's end (after the context exists).
void installContextClient();
// The words for WebKit's termination reasons, in the markers.
const char* terminationReason(uint32_t reason);
std::string activeUrl(View& view);
std::string committedUrl(View& view);
std::string pageTitle(View& view);

// ---- children.cpp: the engine processes the launcher started -------------------------------------

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
// SIGKILL of every engine process the launcher has not seen end (their process groups too), from any
// thread and without waiting: for a host whose WebKit thread hung and which is about to exit, so that it
// does not leave them behind (children.cpp).
void killChildrenNow();
// Called by the clients when a main-frame navigation starts: the pending dialogs of the page that
// is going away are answered "no" and forgotten (their listeners die with the page).
void dropDialogs(View& view);

// ---- policy.cpp: navigation policy and downloads -------------------------------------------------

void decidePolicyForNavigationAction(WKPageRef, WKNavigationActionRef, WKFramePolicyListenerRef, WKTypeRef, const void*);
void decidePolicyForNavigationResponse(WKPageRef, WKNavigationResponseRef, WKFramePolicyListenerRef, WKTypeRef, const void*);
void navigationActionDidBecomeDownload(WKPageRef, WKNavigationActionRef, WKDownloadRef, const void*);
void navigationResponseDidBecomeDownload(WKPageRef, WKNavigationResponseRef, WKDownloadRef, const void*);
// The page's downloads end with the page.
void dropDownloads(View& view);
// Every navigation held for the owner is refused (a newer main-frame navigation, the view's end).
void dropHeldNavigations(View& view);

// ---- uiclient.cpp: the page's UI client and the view's pointer -----------------------------------

void installUiClient(View& view);
// The view client's pointer-shape callback (engine.cpp installs it with the others).
void viewSetCursor(WKViewRef view, uint32_t cursorType, const void* clientInfo);

// ---- light.cpp: light views (docs/webd-plan.md 3.6) ----------------------------------------------

// The rule list that blocks every load but data:, blob: and about: URLs is compiled once for the process
// (WebKit's content-extension store, in a folder of its own: $LAZYOS_WEBD_RULES_DIR or
// /tmp/webd-rules-<pid>). Call after startHost, with the run loop about to turn; a light view cannot be
// made before it is ready (createView returns null), so an owner waits with whenLightLockSettled.
enum class LightLock { Pending, Ready, Failed };
void prepareLightLock();
LightLock lightLock();
// Runs `task` on the main thread once the lock is Ready or Failed (at once, if it already is).
void whenLightLockSettled(std::function<void()> task);
// The lock's compiled list once it is Ready (null before): a full view with ViewConfig::noNetwork takes it too.
WKUserContentFilterRef lightLockFilter();
// Before the view exists: the preferences (scripts, font size, no storage, no windows) and the rule list.
// False when the lock is not ready (the view must not be made).
bool configureLight(View& view, WKPageConfigurationRef configuration);
// The navigation client's decisions for a light view (policy.cpp calls them for a view with `light`).
void lightNavigationAction(View& view, WKNavigationActionRef action, WKFramePolicyListenerRef listener);
void lightNavigationResponse(View& view, WKNavigationResponseRef response, WKFramePolicyListenerRef listener);

// ---- notify.cpp: the Notifications API -----------------------------------------------------------

// Registers WebKit's notification provider and tells it the decisions the owner kept (once; the host
// does it at start when its features ask, and the first view that uses notifications does it otherwise).
// After the context exists.
void installNotifications();
// The page UI client's permission request callback (uiclient.cpp installs it).
void notificationPermissionRequest(WKPageRef, WKSecurityOriginRef, WKNotificationPermissionRequestRef, const void*);
// The page that asked is going away: its pending requests are refused.
void dropNotificationRequests(View& view);
// The owner's answers (the C ABI's lzw_engine_notify_*): run on the main thread.
void notifyShown(uint64_t id);
void notifyClicked(uint64_t id);
void notifyClosed(uint64_t id);
void notifyPermission(const std::string& origin, int answer);
// The same answer for `view`'s pending requests of `origin` only (webd: views of several owners); WebKit's
// policy, which is the context's, is told as notifyPermission tells it.
void notifyPermissionIn(View& view, const std::string& origin, int answer);
void notifyForget(const std::string& origin);

// ---- blocking.cpp: content-extension rule lists (issue #845) -------------------------------------

// Before the view exists, with LZW_FEATURE_BLOCKING: the page's user content controller and the store
// below the profile. Nothing happens without the feature.
void startBlocking(View& view, WKPageConfigurationRef configuration);
// A navigation is about to begin: the rule lists go on the page or off it as the owner says.
// Only a main frame decides; a frame's own navigation keeps the page's state.
void blockingNavigation(View& view, const std::string& address, bool mainFrame);
// The rule lists apply or not as the page on show says (after a navigation that did not replace it:
// a download, a failed load): the state the last decision left is put right.
void blockingSettle(View& view);
// The main frame committed (the count of blocked loads starts again) and finished loading.
void blockingCommitted(View& view);
void blockingFinished(View& view);
// The navigation client's callback for what the rule lists did (patch 0018).
void contentRuleListNotification(WKPageRef, WKURLRef, WKArrayRef, WKArrayRef, const void*);
// The owner's calls on a profile's lists (the C ABI's lzw_engine_blocking_*): main thread.
void blockingLoad(View& view, const std::string& id);
void blockingCompile(View& view, const std::string& id, const std::string& json);
void blockingForget(View& view, const std::string& id);

// ---- paint.cpp -----------------------------------------------------------------------------------

// All of the view is painted next (a caller that names no rectangle).
void schedulePaint(View& view);
// The view's own callback: the rectangle that changed is the one painted.
void viewNeedsDisplay(WKViewRef view, WKRect rect, const void* clientInfo);
// Asks the page for what each update scrolled and repainted (WKPageSetUpdateCallback, patch 0024),
// once the view exists and has its size. Without it the page reports the whole view for a scroll.
void installPaintObserver(View& view);
// Input counted for the once-a-second INFO:PAINT line (a wheel turn also starts the clock the line's
// `w2d` is measured with).
void notePointer(View& view, bool wheel);
// The page's process is gone: the next paint starts from white and covers the whole view.
void invalidatePaint(View& view);
// The view is going: its pacer thread is ended and its bitmaps freed.
void releasePaint(View& view);

} // namespace webkitapp
