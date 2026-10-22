// WebKit's UI-process half (docs/webkit-port-plan.md; docs/lazywebplus.md; docs/webd-plan.md).
//
// Everything here runs on WebKit's main thread (the run loop main() starts). The owner's thread
// calls the View methods through onView(), which only queues a task for this thread (the C ABI of
// lazyos_ui.h does it in legacy.cpp). WebKit is used through its C API alone, as webkit-smoke does:
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
            || other.config.noNetwork != config.noNetwork)
            continue;
        if (!other.crashed && !other.recovering && WKPageGetProcessIdentifier(other.page) > 0)
            return &other;
        any = any ? any : &other;
    }
    return any;
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
    if (!config.light && config.configured && !config.userAgentSuffix.empty())
        applyUserAgentSuffix(view);

    WKViewClientV0 viewClient = { };
    viewClient.base.version = 0;
    viewClient.base.clientInfo = &view;
    viewClient.setViewNeedsDisplay = viewNeedsDisplay;
    if (view.has(LZW_FEATURE_PAGE_UI))
        viewClient.setCursor = viewSetCursor;
    WKViewSetViewClient(view.view, &viewClient.base);
    WKViewSetSize(view.view, WKSizeMake(view.width, view.height));
    installPaintObserver(view);
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
    if (view->page) {
        WKViewSetVisible(view->view, false);
        WKPageStopLoading(view->page);
        // Its process goes with it (107 MB), unless a view that is staying uses it too; the page proxy
        // stays, hollow (see below). In a process that stays the page is unloaded instead: it must not
        // go on running (timers, requests, a busy loop) for nobody, and its heap is given back.
        if (!processMates(*view).empty()) {
            WKURLRef blank = WKURLCreateWithUTF8CString("about:blank");
            WKPageLoadURL(view->page, blank);
            WKRelease(blank);
        } else if (WKPageGetProcessIdentifier(view->page) > 0) {
            WKPageTerminate(view->page);
        }
    }
    releasePaint(*view);
    for (auto it = H.notificationViews.begin(); it != H.notificationViews.end();) {
        if (it->second == id)
            it = H.notificationViews.erase(it);
        else
            ++it;
    }
    // Parked, never released: WebKit's WebPageProxy aborts the UI process when it is destroyed on this
    // port (a CheckedPtr to a dead timer in the run loop, one run in three in webkit-views two; the family
    // of patches 0021 and 0027). A proxy per view that ever was, and the store of a view that had one of
    // its own; no bitmap, no thread, and a process only when a group's other views still use it (the
    // page itself is blank by then).
    H.parked.push_back(std::move(H.views[id]));
    H.views.erase(id);
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
        // by destroying its views (the last one ends the process) and making new ones.
        say("WEBPROCESS:RECOVER:SHARED:%d", static_cast<int>(WKPageGetProcessIdentifier(page)));
        return;
    }
    recovering = true;
    recoverReload = reload;
    say("WEBPROCESS:RECOVER:%d:%s", static_cast<int>(WKPageGetProcessIdentifier(page)), reload ? "reload" : "stop");
    // The termination comes back as webProcessDidTerminate (clients.cpp), which carries on.
    WKPageTerminate(page);
}

void View::load(const std::string& address)
{
    WKURLRef wkUrl = WKURLCreateWithUTF8CString(address.c_str());
    sayHarness("NAV:%s", oneLine(address).c_str());
    WKPageLoadURL(page, wkUrl);
    WKRelease(wkUrl);
}

void View::reload() { WKPageReload(page); }

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

void View::wheel(int x, int y, int ticksX, int ticksY, uint32_t mods)
{
    notePointer(*this, true);
    // One call per notch: the page takes the sign of each axis and scrolls a line step.
    for (int i = 0; i < (ticksY < 0 ? -ticksY : ticksY) && i < 8; ++i)
        WKPageHandleWheelEvent(page, WKWheelEventMake(kWKEventWheel, WKPointMake(x, y), WKSizeMake(0, ticksY < 0 ? -1 : 1), WKSizeMake(0, 0), mods));
    // xui's positive x is to the right; WebKit's is to the left (as GTK's GDK_SCROLL_LEFT is +1).
    for (int i = 0; i < (ticksX < 0 ? -ticksX : ticksX) && i < 8; ++i)
        WKPageHandleWheelEvent(page, WKWheelEventMake(kWKEventWheel, WKPointMake(x, y), WKSizeMake(ticksX < 0 ? 1 : -1, 0), WKSizeMake(0, 0), mods));
}

void View::key(bool press, uint32_t keysym, const std::string& typed, uint32_t mods)
{
    WKKeyboardEvent event = WKKeyboardEventMake(press ? kWKEventKeyDown : kWKEventKeyUp, kWKInputTypeNormal,
        typed.c_str(), static_cast<uint32_t>(typed.size()), nullptr, static_cast<int32_t>(keysym), 0, 0, mods);
    WKPageHandleKeyboardEvent(page, event);
}

void View::focus(bool focused) { WKViewSetFocus(view, focused); }

} // namespace webkitapp
