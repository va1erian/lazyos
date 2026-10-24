// WebKit's UI-process half (docs/webkit-port-plan.md; docs/lazyweb.md; docs/webd-plan.md).
//
// Everything here runs on WebKit's main thread (the run loop main() starts). The owner's thread
// calls the View methods through onView(), which only queues a task for this thread (webd's abi.cpp
// does it). WebKit is used through its C API alone, as webkit-smoke does:
// a context pointed at the WebProcess and NetworkProcess binaries, a page configuration per view,
// the PlayStation port's WKView; the page is painted into a bitmap with WKPagePaint (paint.cpp) and
// handed to the view's sink; mouse, wheel and key events go in through WKPageHandle*Event. What the
// page says back, and the markers, are in clients.cpp.
//
// One WKContext serves every view; a view's data store is its profile directory's, shared by the
// views of that directory, or a non-persistent store of its own (shared by the views of a group).

#include <WebKit/WKBase.h>
#include <WebKit/WKContext.h>
#include <WebKit/WKContextConfigurationPlayStation.h>
#include <WebKit/WKContextConfigurationRef.h>
#include <WebKit/WKGeometry.h>
#include <WebKit/WKPage.h>
#include <WebKit/WKPageConfigurationRef.h>
#include <WebKit/WKPagePrivate.h>
#include <WebKit/WKPagePrivatePlayStation.h>
#include <WebKit/WKRunLoop.h>
#include <WebKit/WKString.h>
#include <WebKit/WKType.h>
#include <WebKit/WKURL.h>
#include <WebKit/WKView.h>
#include <WebKit/WKViewClient.h>
#include <WebKit/WKWebsiteDataStoreConfigurationRef.h>
#include <WebKit/WKWebsiteDataStoreRef.h>

#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <string>
#include <sys/stat.h>
#include <unistd.h>

#include "engine.h"
#include "lazyos_ui.h"

// The build sets these from libs/fhs (tools/webkit/port_cmake.py, WebKit/PlatformLazyOS.cmake).
// The shipped browser execs exactly these: no environment variable chooses what it runs.
#ifndef LAZYOS_WEBPROCESS_PATH
#error "LAZYOS_WEBPROCESS_PATH must name the image's webprocess (libs/fhs)"
#endif
#ifndef LAZYOS_NETWORKPROCESS_PATH
#error "LAZYOS_NETWORKPROCESS_PATH must name the image's networkprocess (libs/fhs)"
#endif

namespace webkitapp {

Host H;

View* viewById(int id)
{
    auto it = H.views.find(id);
    return it == H.views.end() ? nullptr : it->second.get();
}

View* viewOfPage(WKPageRef page)
{
    for (auto& entry : H.views) {
        if (entry.second->page == page)
            return entry.second.get();
    }
    return nullptr;
}

namespace {

// Tasks from other threads: queued on the run loop, run once, freed.
void runTask(void* context)
{
    auto* task = static_cast<std::function<void()>*>(context);
    (*task)();
    delete task;
}

void clearClients(View& view)
{
    if (!view.page)
        return;
    WKPageSetPageNavigationClient(view.page, nullptr);
    WKPageSetPageStateClient(view.page, nullptr);
    WKPageSetPageUIClient(view.page, nullptr);
    // The update, scroll-offset and band callbacks are the process's, not the page's (paint.cpp): the
    // other views still use them.
    if (view.view)
        WKViewSetViewClient(view.view, nullptr);
}

} // namespace

void onMain(std::function<void()> task)
{
    WKRunLoopCallOnMainThread(runTask, new std::function<void()>(std::move(task)));
}

void onView(int id, std::function<void(View&)> task)
{
    onMain([id, task = std::move(task)] {
        if (View* view = viewById(id))
            task(*view);
    });
}

bool startHost(HostEnv* env, const std::string& profileHint)
{
    H.env = env;
    elapsedMs();
    // The profile directory first: it goes into the environment the children inherit, and nothing
    // may start a process before it is there.
    if (!profileHint.empty() && makePrivateDirectories(profileHint))
        exportProfileEnvironment(profileHint);
    // The page's process paints a view's height of the page above and below the view ahead of a
    // scroll (WebKit patch 0030, paint.cpp "bands"); a harness may set another amount, 0 for none.
    setenv("LAZYOS_WEBKIT_OVERSCAN", "1", 0);
    // On one CPU JavaScriptCore's concurrent collector has no second core to mark on: its helper
    // threads take turns with the page's main thread, which the collector also slows down while it
    // marks, and a scroll stalls behind both. A stop-the-world collection is shorter there.
    if (sysconf(_SC_NPROCESSORS_ONLN) <= 1)
        setenv("JSC_useConcurrentGC", "false", 0);
    WKContextConfigurationRef configuration = WKContextConfigurationCreate();
    WKStringRef webPath = WKStringCreateWithUTF8CString(LAZYOS_WEBPROCESS_PATH);
    WKStringRef networkPath = WKStringCreateWithUTF8CString(LAZYOS_NETWORKPROCESS_PATH);
    WKContextConfigurationSetWebProcessPath(configuration, webPath);
    WKContextConfigurationSetNetworkProcessPath(configuration, networkPath);
    // One WebProcess for every page of a data store, started with the view: a cross-site navigation
    // does not get a fresh process (108 MB loaded whole into RAM, a JIT region and heaps of its own:
    // the second one's `exec` failed with ENOMEM under the user's memory quota), and none is kept warm.
    WKContextConfigurationSetProcessSwapsOnNavigation(configuration, false);
    WKContextConfigurationSetPrewarmsProcessesAutomatically(configuration, false);
    WKContextConfigurationSetUsesWebProcessCache(configuration, false);
    H.context = WKContextCreateWithConfiguration(configuration);
    // The notification provider before any page process: it reads the decisions when it starts.
    if (has(LZW_FEATURE_NOTIFICATIONS))
        installNotifications();
    installContextClient();
    return H.context != nullptr;
}

bool startHostWithChildren(HostEnv* env, const std::string& childrenDirectory)
{
    // The owner first: the environment's markers go to it.
    H.env = env;
    return fixChildrenEnvironment(childrenDirectory) && startHost(env);
}

// The other live views whose page runs in the same WebProcess as `view`'s (a group's). A crashed view
// keeps the id of the process that died, which the kernel may give to a new one: it is not counted.
static std::vector<View*> processMates(const View& view)
{
    std::vector<View*> mates;
    const WKProcessID process = view.page ? WKPageGetProcessIdentifier(view.page) : 0;
    if (process <= 0)
        return mates;
    for (auto& entry : H.views) {
        View& other = *entry.second;
        if (other.id != view.id && other.page && !other.crashed && WKPageGetProcessIdentifier(other.page) == process)
            mates.push_back(&other);
    }
    return mates;
}

// A live view the new one joins: the same group and profile directory, with a page. One whose process
// runs is preferred: a related page of a dead process would start another one for it.
static View* groupMate(const ViewConfig& config)
{
    if (config.group == 0)
        return nullptr;
    View* any = nullptr;
    for (auto& entry : H.views) {
        View& other = *entry.second;
        if (!other.page || other.config.group != config.group || other.config.profile != config.profile
            || other.config.memoryOnly != config.memoryOnly || other.config.light != config.light
            // A page with no network must not be related to one that has it (related pages script each other).
            || other.config.noNetwork != config.noNetwork
            // A client's own in-memory profile and its private window have the same group, no directory and
            // memoryOnly: the store kind is all that tells their data apart (webd_abi.h, WEBD_STORE_*).
            || other.config.storeKind != config.storeKind)
            continue;
        if (!other.crashed && !other.recovering && WKPageGetProcessIdentifier(other.page) > 0)
            return &other;
        any = any ? any : &other;
    }
    return any;
}

// Which destroyed views may go to the reuse pool: LIGHT views only, of a real group (webd gives every
// client one, never reused; group 0 is "no group" and could never be adopted).
//
// A full view is never pooled. Its proxy carries state a new view must not inherit and that this port
// cannot reset: an in-memory store (a private window's or the client's own profile: its cookies and
// storage would survive into the next private window, where a fresh view gets a fresh store), the back
// and forward list (Back, Reload and recover would act on the closed page), and the page's content
// controller (a view with content blocking would keep the old lists, unknown to the new View). A light
// view has none of these through its interface: no history, no Back/Reload/Recover, no blocking (its
// features are 0), and every page is the client's own LoadHtml; its in-memory store is the same
// client's, as it is for the one view Mail and Docs keep for every page. Found in the pre-PR review.
static bool poolable(const View& view)
{
    return view.config.light && view.config.group != 0 && view.soleEver;
}

// Two views are of one class when a proxy configured for one can serve the other: the same owner's
// group and profile, and everything baked into the WKPage/WKView at creation (data store, content
// controller, the light or full configuration, the user agent). Width and height are not here: a
// reused view is resized. Views of one class share a web process already, so reusing a proxy across
// the class is no weaker a wall than the group is.
static bool sameViewClass(const ViewConfig& a, const ViewConfig& b)
{
    return a.group == b.group && a.group != 0 && a.profile == b.profile && a.memoryOnly == b.memoryOnly
        && a.light == b.light && a.noNetwork == b.noNetwork && a.storeKind == b.storeKind
        && a.features == b.features && a.configured == b.configured && a.scripts == b.scripts
        && a.colorScheme == b.colorScheme && a.defaultFontSize == b.defaultFontSize
        && a.userStyle == b.userStyle && a.userAgentSuffix == b.userAgentSuffix && a.userAgent == b.userAgent;
}

// The pool's capacity: a client reuses the same few classes (its own profile, a private one, the
// browser profile), so this is generous. An overflow proxy is retired, never released.
static constexpr size_t kPoolMax = 16;

static void sayPool()
{
    say("INFO:POOL:reused=%llu:pooled=%zu:live=%zu:retired=%zu:parked=%zu",
        static_cast<unsigned long long>(H.poolReused), H.pool.size(), H.views.size(), H.retired.size(),
        H.parked.size());
}

// A pooled proxy of `config`'s class, removed from the pool, or an empty ParkedPage (null view) when
// there is none.
static ParkedPage takeFromPool(const ViewConfig& config)
{
    for (auto it = H.pool.begin(); it != H.pool.end(); ++it) {
        // Only once the parked page has no process: its termination has been taken in, so no late
        // notice of it reaches the clients the new view installs (a page still ending stays pooled).
        if (sameViewClass(it->config, config) && WKPageGetProcessIdentifier(it->page) <= 0) {
            ParkedPage taken = std::move(*it);
            H.pool.erase(it);
            return taken;
        }
    }
    return ParkedPage {};
}

// Keep a destroyed view's proxy for reuse by a later view of its class (its process was ended by the
// caller). The oldest proxy is retired when the pool is full. The view's view/page are taken, so its
// destructor releases nothing.
static void parkForReuse(View& view)
{
    ParkedPage parked;
    parked.view = view.view;
    parked.page = view.page;
    parked.profile = view.profile;
    parked.config = view.config;
    view.view = nullptr;
    view.page = nullptr;
    if (H.pool.size() >= kPoolMax) {
        H.retired.push_back(std::move(H.pool.front()));
        H.pool.erase(H.pool.begin());
    }
    H.pool.push_back(std::move(parked));
}

View* createView(const ViewConfig& config, ViewSink* sink)
{
    // A light view without its network lock would be a browser view: never made (light.cpp).
    if (config.light && lightLock() != LightLock::Ready) {
        say("FAIL:view:the light network lock is not ready");
        return nullptr;
    }
    // None of a browser's clients for a light view, whatever the owner asked for.
    ViewConfig effective = config;
    if (config.light)
        effective.features = 0;
    auto owned = std::make_unique<View>(H.nextView++, effective, sink);
    View& view = *owned;
    View* mate = groupMate(config);
    // Reuse a parked proxy of the same owner and class when no live mate shares its process already: a
    // WebPageProxy cannot be released on this port, so without reuse a create/destroy loop grows the
    // process without bound (docs/webd-plan.md P5). A live mate is preferred (its process is warm); the
    // pool is drained only when the group would otherwise start a process, which is exactly the loop
    // that grew webd.
    // A view that joins a live process (a related page of its mate) must never be pooled for reuse, and
    // neither must the mate: both now share a process, which makes reusing either one's proxy later
    // unsafe (webkit-views group).
    if (mate) {
        view.soleEver = false;
        mate->soleEver = false;
    }
    ParkedPage reused = mate ? ParkedPage {} : takeFromPool(effective);
    if (reused.view) {
        // The proxy keeps its data store, content controller and light/full configuration from when it
        // was made (same class): adopt them. Its process was ended at park; the owner's first load
        // starts a fresh one. Its clients were cleared at destroy and are installed again below.
        view.profile = reused.profile;
        view.view = reused.view;
        view.page = reused.page;
        ++H.poolReused;
        sayPool();
    } else {
        // A group shares the mate's store (an ephemeral one is the mate's own, there is no other way to
        // name it), else the profile directory's or a new one.
        view.profile = mate ? mate->profile : profileFor(config.profile, config.memoryOnly);

        WKPageConfigurationRef pageConfiguration = WKPageConfigurationCreate();
        WKPageConfigurationSetContext(pageConfiguration, H.context);
        WKPageConfigurationSetWebsiteDataStore(pageConfiguration, view.profile->store);
        // Related pages share a WebProcess (WebPageProxy::create takes the related page's process).
        if (mate)
            WKPageConfigurationSetRelatedPage(pageConfiguration, mate->page);
        // The provider, for an owner whose host features did not ask for it but whose view does.
        if (view.has(LZW_FEATURE_NOTIFICATIONS))
            installNotifications();
        // Content blocking (LZW_FEATURE_BLOCKING): the page's user content controller, before the view.
        startBlocking(view, pageConfiguration);
        // A light view's preferences and network lock, before the view too.
        if (config.light && !configureLight(view, pageConfiguration)) {
            say("FAIL:view:the light view could not be configured");
            return nullptr;
        }
        // A full view whose owner configured it (webd): its scripts, font size, style and network lock, before the
        // view too.
        if (!config.light && config.configured && !configureFull(view, pageConfiguration))
            return nullptr;
        // The configuration is not released, as before: the view keeps what it needs of it.
        view.view = WKViewCreate(pageConfiguration);
        if (!view.view) {
            say("FAIL:view:WKViewCreate returned null");
            return nullptr;
        }
        view.page = WKViewGetPage(view.view);
        if (!config.light && config.configured && (!config.userAgentSuffix.empty() || !config.userAgent.empty()))
            applyUserAgent(view);
    }

    WKViewClientV0 viewClient = { };
    viewClient.base.version = 0;
    viewClient.base.clientInfo = &view;
    viewClient.setViewNeedsDisplay = viewNeedsDisplay;
    if (view.has(LZW_FEATURE_PAGE_UI))
        viewClient.setCursor = viewSetCursor;
    WKViewSetViewClient(view.view, &viewClient.base);
    WKViewSetSize(view.view, WKSizeMake(view.width, view.height));
    installPaintObserver(view);
    installWheelObserver(view);
    WKViewSetActive(view.view, true);
    WKViewSetFocus(view.view, true);
    WKViewSetVisible(view.view, true);

    installClients(view);
    if (view.has(LZW_FEATURE_PAGE_UI | LZW_FEATURE_DIALOGS | LZW_FEATURE_NOTIFICATIONS))
        installUiClient(view);
    H.views[view.id] = std::move(owned);
    return &view;
}

void destroyView(int id)
{
    View* view = viewById(id);
    if (!view)
        return;
    // Nothing the page says reaches a view that is going: its clients are cleared, then its questions
    // get the answers a closed page gives.
    clearClients(*view);
    dropDialogs(*view);
    dropDownloads(*view);
    dropHeldNavigations(*view);
    // Whether a view that is staying shares this view's process: then the process cannot be ended and
    // the proxy cannot be pooled (it is a related page of a live process), so it is parked forever.
    bool shared = false;
    // The page's process is known to be over (ended here) or never began: only then may the proxy be
    // reused. A view destroyed while its first process is still launching (no id yet, but a load began)
    // is not: the next owner would inherit a process and a page mid-load.
    bool processSettled = false;
    if (view->page) {
        WKViewSetVisible(view->view, false);
        WKPageStopLoading(view->page);
        shared = !processMates(*view).empty();
        const bool began = view->loadRequested || view->webProcessSeen || view->loading;
        // Its process goes with it (107 MB), unless a view that is staying uses it too; the page proxy
        // stays, hollow (see below). In a process that stays the page is unloaded instead: it must not
        // go on running (timers, requests, a busy loop) for nobody, and its heap is given back.
        if (shared) {
            WKURLRef blank = WKURLCreateWithUTF8CString("about:blank");
            WKPageLoadURL(view->page, blank);
            WKRelease(blank);
        } else if (WKPageGetProcessIdentifier(view->page) > 0) {
            WKPageTerminate(view->page);
            processSettled = true;
        } else if (began) {
            // A load was asked for and WebKit has no process id yet: the first process may be launching.
            // End it too, or it would run for nobody (107 MB) behind a parked proxy; a client looping
            // CreateView, LoadHtml, DestroyView would pile them up (found by the P5 review). The proxy is
            // not pooled: its process is not known to be over.
            WKPageTerminate(view->page);
        } else {
            processSettled = true;
        }
    }
    releasePaint(*view);
    for (auto it = H.notificationViews.begin(); it != H.notificationViews.end();) {
        if (it->second == id)
            it = H.notificationViews.erase(it);
        else
            ++it;
    }
    // A WebPageProxy cannot be released on this port: WebKit aborts the UI process when it is destroyed
    // (a CheckedPtr to a dead timer in the run loop, one run in three in webkit-views two; the family of
    // patches 0021 and 0027). So the proxy is kept. A light view that was the only page of its process
    // all its life, whose process is over, goes to the reuse pool, where the same client's next light
    // view of the same class adopts it instead of starting a fresh proxy (`poolable` says why only light
    // views). Everything else is parked forever as before: full views, views whose process a sibling
    // still used (a related page of a live process), group 0, a view destroyed mid-launch. Those grow
    // with use, as before P5; the pool bounds a client's light create/destroy loop. No bitmap, no thread.
    if (!shared && processSettled && poolable(*view) && view->view && view->page) {
        parkForReuse(*view);
        H.views.erase(id);
    } else {
        H.parked.push_back(std::move(H.views[id]));
        H.views.erase(id);
    }
    // After the view is gone from the table, so `live` counts what is left.
    sayPool();
}

// ---- the owner's calls ------------------------------------------------------------------------------

void View::recover(bool reload)
{
    if (!page)
        return;
    if (WKPageGetProcessIdentifier(page) <= 0) {
        // No process to end (it is gone already): reload starts one.
        if (reload)
            WKPageReload(page);
        return;
    }
    if (!processMates(*this).empty()) {
        // Not for a process other views use: ending it and reloading its pages aborted the UI process in
        // most runs of webkit-views group (WebPageProxy::launchProcess -> WebProcessProxy::removeWebPage ->
        // updateMediaStreamingActivity, a run-loop timer with a CheckedPtr to a dead object; the family
        // of patches 0021 and 0027), and a reload splits the group in any case. The owner recovers a group
        // by destroying its views (the last one ends the process) and making new ones. (View::recoverGroup ends the
        // process without a reload; it was written without a build volume to run it, so whether termination alone
        // is safe here is unproven: run the `group` case of webkit-views before relying on it.)
        say("WEBPROCESS:RECOVER:SHARED:%d", static_cast<int>(WKPageGetProcessIdentifier(page)));
        return;
    }
    recovering = true;
    recoverReload = reload;
    say("WEBPROCESS:RECOVER:%d:%s", static_cast<int>(WKPageGetProcessIdentifier(page)), reload ? "reload" : "stop");
    // The termination comes back as webProcessDidTerminate (clients.cpp), which carries on.
    WKPageTerminate(page);
}

void View::recoverGroup()
{
    if (!page)
        return;
    const WKProcessID process = WKPageGetProcessIdentifier(page);
    if (process <= 0)
        return;
    // Every view in the process hears of its end (webProcessDidTerminate): the ones that asked for nothing are
    // marked as the initiator is, so each takes the "stopped on request" way (no reload: reloading into a process
    // that others share aborted the UI process, see View::recover) and webd reports them with WEBD_LOST_OTHER.
    const std::vector<View*> mates = processMates(*this);
    for (View* mate : mates) {
        mate->recovering = true;
        mate->recoverReload = false;
    }
    recovering = true;
    recoverReload = false;
    say("WEBPROCESS:RECOVER:GROUP:%d:%d", static_cast<int>(process), static_cast<int>(mates.size() + 1));
    WKPageTerminate(page);
}

void View::load(const std::string& address)
{
    loadRequested = true;
    WKURLRef wkUrl = WKURLCreateWithUTF8CString(address.c_str());
    sayHarness("NAV:%s", oneLine(address).c_str());
    WKPageLoadURL(page, wkUrl);
    WKRelease(wkUrl);
}

void View::reload()
{
    loadRequested = true;
    WKPageReload(page);
}

void View::stop()
{
    // The error WebKit reports for this is the user's own doing, not a failure.
    stopRequested = loading;
    WKPageStopLoading(page);
}

void View::back() { WKPageGoBack(page); }
void View::forward() { WKPageGoForward(page); }

void View::resize(int newWidth, int newHeight)
{
    static bool windowSeen = false;
    if (!windowSeen) {
        windowSeen = true;
        say("INFO:TIME:window=%ld:size=%dx%d", elapsedMs(), newWidth, newHeight);
    }
    if (newWidth <= 0 || newHeight <= 0 || (newWidth == width && newHeight == height))
        return;
    width = newWidth;
    height = newHeight;
    say("INFO:RESIZE:%dx%d", newWidth, newHeight);
    WKViewSetSize(view, WKSizeMake(newWidth, newHeight));
    schedulePaint(*this);
}

void View::mouse(int kind, int button, int x, int y, uint32_t mods)
{
    notePointer(*this, false);
    WKEventType type = kind == 0 ? kWKEventMouseDown : kind == 1 ? kWKEventMouseUp : kWKEventMouseMove;
    WKEventMouseButton which = button == 0 ? kWKEventMouseButtonLeftButton
        : button == 1                      ? kWKEventMouseButtonMiddleButton
        : button == 2                      ? kWKEventMouseButtonRightButton
                                           : kWKEventMouseButtonNoButton;
    WKPageHandleMouseEvent(page, WKMouseEventMake(type, which, WKPointMake(x, y), 1, mods));
}

void View::key(bool press, uint32_t keysym, const std::string& typed, uint32_t mods)
{
    WKKeyboardEvent event = WKKeyboardEventMake(press ? kWKEventKeyDown : kWKEventKeyUp, kWKInputTypeNormal,
        typed.c_str(), static_cast<uint32_t>(typed.size()), nullptr, static_cast<int32_t>(keysym), 0, 0, mods);
    WKPageHandleKeyboardEvent(page, event);
}

void View::focus(bool focused) { WKViewSetFocus(view, focused); }

} // namespace webkitapp
