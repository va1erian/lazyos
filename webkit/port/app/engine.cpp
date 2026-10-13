// WebKit's UI-process half for the desktop window (docs/webkit-port-plan.md, W5).
//
// Everything here runs on WebKit's main thread (the run loop main() starts). The window's
// thread calls the lzw_engine_* functions, each of which only queues a task for this
// thread (lazyos_ui.h). WebKit is used through its C API alone, as webkit-smoke does:
// a context pointed at the WebProcess and NetworkProcess binaries, a page configuration,
// the PlayStation port's WKView; the page is painted into a bitmap with WKPagePaint
// (paint.cpp) and handed to the window; mouse, wheel and key events go in through
// WKPageHandle*Event. What the page says back, and the markers, are in clients.cpp.

#include <WebKit/WKBase.h>
#include <WebKit/WKContext.h>
#include <WebKit/WKContextConfigurationPlayStation.h>
#include <WebKit/WKContextConfigurationRef.h>
#include <WebKit/WKGeometry.h>
#include <WebKit/WKPage.h>
#include <WebKit/WKPageConfigurationRef.h>
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
#include <signal.h>
#include <stdint.h>
#include <stdlib.h>
#include <string>
#include <sys/stat.h>
#include <sys/wait.h>
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
// The app's per-user folder is `<home>/<LAZYOS_APP_DATA_DIR>/<LAZYOS_WEBKIT_APP_ID>` (libs/fhs
// `app_data_dir`; the package's `write:$HOME/.apps/os.lazy.webkit` rule names exactly it).
#ifndef LAZYOS_APP_DATA_DIR
#error "LAZYOS_APP_DATA_DIR must name the per-user app data directory (libs/fhs state::APP_DATA_DIR)"
#endif
#ifndef LAZYOS_WEBKIT_APP_ID
#error "LAZYOS_WEBKIT_APP_ID must be this package's system_name (xui-app/packages/webkit)"
#endif

namespace webkitapp {

Engine E;

namespace {

// Tasks from other threads: queued on the run loop, run once, freed.
void runTask(void* context)
{
    auto* task = static_cast<std::function<void()>*>(context);
    (*task)();
    delete task;
}

} // namespace

void onMain(std::function<void()> task)
{
    WKRunLoopCallOnMainThread(runTask, new std::function<void()>(std::move(task)));
}

namespace {

// The cookie jar, below the profile directory (a SQLite database the NetworkProcess owns).
constexpr const char* kCookieJar = "cookies/cookie.jar.db";

// `mkdir -p`, private to the user: what this browser keeps is the user's alone.
bool makeDirectories(const std::string& path)
{
    for (size_t at = 1; at <= path.size(); ++at) {
        if (at != path.size() && path[at] != '/')
            continue;
        const std::string prefix = path.substr(0, at);
        if (mkdir(prefix.c_str(), 0700) != 0 && errno != EEXIST)
            return false;
    }
    return true;
}

// The user's profile directory, `<home>/.apps/os.lazy.webkit`: the app's own per-user folder
// (`init` gives every app its `HOME`), and the one place the browser's rules let any of its
// processes write. Empty when there is no home: the browser then keeps nothing at all.
std::string profileDirectory()
{
    const char* home = getenv("HOME");
    if (!home || *home != '/')
        return std::string();
    std::string root(home);
    while (root.size() > 1 && root.back() == '/')
        root.pop_back();
    return root + "/" LAZYOS_APP_DATA_DIR "/" LAZYOS_WEBKIT_APP_ID;
}

// The website data store the page lives in: everything WebKit keeps (cookies, the HTTP cache,
// local storage, IndexedDB, ...) below `profile`; WebKit's own defaults for the directories this
// does not name follow the profile too (patch 0015, through LAZYOS_WEBKIT_PROFILE). No profile
// gives an ephemeral store, never a directory of the working directory.
WKWebsiteDataStoreRef createDataStore(const std::string& profile)
{
    if (profile.empty() || !makeDirectories(profile)) {
        say("WEBKIT:W5:PROFILE:NONE:the browser keeps nothing");
        return WKWebsiteDataStoreCreateNonPersistentDataStore();
    }
    // The children inherit these: the launcher starts them in the profile directory, WebKit's
    // default data directories (patch 0015) are below it, and curl opens its cookie database at
    // CURL_COOKIE_JAR_PATH (the working directory otherwise) before the data store's own file is
    // applied (WebCore/platform/network/curl/NetworkStorageSessionCurl.cpp).
    setenv("LAZYOS_WEBKIT_PROFILE", profile.c_str(), 1);
    setenv("CURL_COOKIE_JAR_PATH", (profile + "/" + kCookieJar).c_str(), 1);

    auto path = [&profile](const char* leaf) {
        return WKStringCreateWithUTF8CString((profile + "/" + leaf).c_str());
    };
    WKWebsiteDataStoreConfigurationRef configuration = WKWebsiteDataStoreConfigurationCreate();
    struct Setting {
        void (*set)(WKWebsiteDataStoreConfigurationRef, WKStringRef);
        const char* leaf;
    };
    const Setting settings[] = {
        { WKWebsiteDataStoreConfigurationSetCookieStorageFile, kCookieJar },
        { WKWebsiteDataStoreConfigurationSetNetworkCacheDirectory, "cache" },
        { WKWebsiteDataStoreConfigurationSetLocalStorageDirectory, "storage/local" },
        { WKWebsiteDataStoreConfigurationSetIndexedDBDatabaseDirectory, "storage/indexeddb" },
        { WKWebsiteDataStoreConfigurationSetWebSQLDatabaseDirectory, "storage/websql" },
        { WKWebsiteDataStoreConfigurationSetCacheStorageDirectory, "storage/cachestorage" },
        { WKWebsiteDataStoreConfigurationSetGeneralStorageDirectory, "storage/general" },
        { WKWebsiteDataStoreConfigurationSetResourceLoadStatisticsDirectory, "storage/statistics" },
        { WKWebsiteDataStoreConfigurationSetServiceWorkerRegistrationDirectory, "storage/serviceworkers" },
    };
    for (const auto& setting : settings) {
        WKStringRef value = path(setting.leaf);
        setting.set(configuration, value);
        WKRelease(value);
    }
    // The directories exist before a process that may only write inside them looks for one.
    for (const char* leaf : { "cookies", "cache", "storage" })
        makeDirectories(profile + "/" + leaf);
    WKWebsiteDataStoreRef store = WKWebsiteDataStoreCreateWithConfiguration(configuration);
    WKRelease(configuration);
    say("WEBKIT:W5:PROFILE:PASS");
    sayHarness("WEBKIT:W5:PROFILE:DIR:%s", oneLine(profile).c_str());
    return store;
}

} // namespace

} // namespace webkitapp

using namespace webkitapp;

extern "C" {

int lzw_engine_start(void)
{
    elapsedMs();
    // The user's profile directory first: it goes into the environment the children inherit,
    // and nothing may start a process before it is there.
    WKWebsiteDataStoreRef dataStore = createDataStore(profileDirectory());
    WKContextConfigurationRef configuration = WKContextConfigurationCreate();
    WKStringRef webPath = WKStringCreateWithUTF8CString(LAZYOS_WEBPROCESS_PATH);
    WKStringRef networkPath = WKStringCreateWithUTF8CString(LAZYOS_NETWORKPROCESS_PATH);
    WKContextConfigurationSetWebProcessPath(configuration, webPath);
    WKContextConfigurationSetNetworkProcessPath(configuration, networkPath);
    // One WebProcess for every page, started with the view: a cross-site navigation does not get
    // a fresh process (108 MB loaded whole into RAM, a JIT region and heaps of its own: the second
    // one's `exec` failed with ENOMEM under the user's memory quota), and none is kept warm.
    WKContextConfigurationSetProcessSwapsOnNavigation(configuration, false);
    WKContextConfigurationSetPrewarmsProcessesAutomatically(configuration, false);
    WKContextConfigurationSetUsesWebProcessCache(configuration, false);
    E.context = WKContextCreateWithConfiguration(configuration);

    WKPageConfigurationRef pageConfiguration = WKPageConfigurationCreate();
    WKPageConfigurationSetContext(pageConfiguration, E.context);
    WKPageConfigurationSetWebsiteDataStore(pageConfiguration, dataStore);
    WKRelease(dataStore);
    E.view = WKViewCreate(pageConfiguration);
    if (!E.view) {
        say("WEBKIT:W5:FAIL:view:WKViewCreate returned null");
        return 1;
    }
    E.page = WKViewGetPage(E.view);

    static WKViewClientV0 viewClient = { };
    viewClient.base.version = 0;
    viewClient.setViewNeedsDisplay = viewNeedsDisplay;
    WKViewSetViewClient(E.view, &viewClient.base);
    WKViewSetSize(E.view, WKSizeMake(E.width, E.height));
    WKViewSetActive(E.view, true);
    WKViewSetFocus(E.view, true);
    WKViewSetVisible(E.view, true);

    installClients();
    return 0;
}

void lzw_engine_quit(void) { WKRunLoopCallOnMainThread([](void*) { WKRunLoopStopMain(); }, nullptr); }

// Is `pid` gone? Reaps it when it is our child; a process that is not ours (or already reaped by
// WebKit's own launcher) is gone when the kernel has no such id.
static bool gone(int pid)
{
    int status = 0;
    const pid_t reaped = waitpid(pid, &status, WNOHANG);
    if (reaped == pid)
        return true;
    return reaped < 0 && errno == ECHILD && kill(pid, 0) < 0 && errno == ESRCH;
}

int lzw_engine_shutdown(void)
{
    struct Child {
        const char* name;
        int pid;
        bool exited;
    } children[] = { { "web", 0, false }, { "network", 0, false } };
    if (E.page && E.context)
        rememberChildren();
    children[0].pid = E.webPid;
    children[1].pid = E.networkPid;
    for (auto& child : children) {
        if (child.pid > 0)
            kill(child.pid, SIGTERM);
    }
    // A grace period for the processes to flush and exit; whatever is left is killed.
    for (int round = 0; round < 2; ++round) {
        for (int waited = 0; waited < 150; ++waited) {
            bool all = true;
            for (auto& child : children) {
                if (child.pid > 0 && !child.exited)
                    child.exited = gone(child.pid);
                all = all && (child.pid <= 0 || child.exited);
            }
            if (all)
                break;
            usleep(20 * 1000);
        }
        if (round == 0) {
            for (auto& child : children) {
                if (child.pid > 0 && !child.exited)
                    kill(child.pid, SIGKILL);
            }
        }
    }
    int left = 0;
    for (auto& child : children) {
        if (child.pid <= 0)
            continue;
        say("WEBKIT:W5:CHILD:%s:%d:%s", child.name, child.pid, child.exited ? "GONE" : "ALIVE");
        left += child.exited ? 0 : 1;
    }
    return left;
}

void lzw_engine_load(const char* url)
{
    std::string address = url ? url : "";
    onMain([address] {
        WKURLRef wkUrl = WKURLCreateWithUTF8CString(address.c_str());
        sayHarness("WEBKIT:W5:NAV:%s", oneLine(address).c_str());
        WKPageLoadURL(E.page, wkUrl);
        WKRelease(wkUrl);
    });
}

void lzw_engine_reload(void) { onMain([] { WKPageReload(E.page); }); }
void lzw_engine_stop(void)
{
    onMain([] {
        // The error WebKit reports for this is the user's own doing, not a failure.
        E.stopRequested = E.loading;
        WKPageStopLoading(E.page);
    });
}
void lzw_engine_back(void) { onMain([] { WKPageGoBack(E.page); }); }
void lzw_engine_forward(void) { onMain([] { WKPageGoForward(E.page); }); }

void lzw_engine_resize(int width, int height)
{
    onMain([width, height] {
        static bool windowSeen = false;
        if (!windowSeen) {
            windowSeen = true;
            say("WEBKIT:W5:INFO:TIME:window=%ld:size=%dx%d", elapsedMs(), width, height);
        }
        if (width <= 0 || height <= 0 || (width == E.width && height == E.height))
            return;
        E.width = width;
        E.height = height;
        say("WEBKIT:W5:INFO:RESIZE:%dx%d", width, height);
        WKViewSetSize(E.view, WKSizeMake(width, height));
        schedulePaint();
    });
}

void lzw_engine_mouse(int kind, int button, int x, int y, uint32_t mods)
{
    onMain([=] {
        WKEventType type = kind == 0 ? kWKEventMouseDown : kind == 1 ? kWKEventMouseUp : kWKEventMouseMove;
        WKEventMouseButton which = button == 0 ? kWKEventMouseButtonLeftButton
            : button == 1                      ? kWKEventMouseButtonMiddleButton
            : button == 2                      ? kWKEventMouseButtonRightButton
                                               : kWKEventMouseButtonNoButton;
        WKPageHandleMouseEvent(E.page, WKMouseEventMake(type, which, WKPointMake(x, y), 1, mods));
    });
}

void lzw_engine_wheel(int x, int y, int ticksX, int ticksY, uint32_t mods)
{
    onMain([=] {
        // One call per notch: the page takes the sign of each axis and scrolls a line step.
        for (int i = 0; i < (ticksY < 0 ? -ticksY : ticksY) && i < 8; ++i)
            WKPageHandleWheelEvent(E.page, WKWheelEventMake(kWKEventWheel, WKPointMake(x, y), WKSizeMake(0, ticksY < 0 ? -1 : 1), WKSizeMake(0, 0), mods));
        // xui's positive x is to the right; WebKit's is to the left (as GTK's GDK_SCROLL_LEFT is +1).
        for (int i = 0; i < (ticksX < 0 ? -ticksX : ticksX) && i < 8; ++i)
            WKPageHandleWheelEvent(E.page, WKWheelEventMake(kWKEventWheel, WKPointMake(x, y), WKSizeMake(ticksX < 0 ? 1 : -1, 0), WKSizeMake(0, 0), mods));
    });
}

void lzw_engine_key(int press, uint32_t keysym, const char* text, uint32_t mods)
{
    std::string typed = text ? text : "";
    onMain([=] {
        WKKeyboardEvent event = WKKeyboardEventMake(press ? kWKEventKeyDown : kWKEventKeyUp, kWKInputTypeNormal,
            typed.c_str(), static_cast<uint32_t>(typed.size()), nullptr, static_cast<int32_t>(keysym), 0, 0, mods);
        WKPageHandleKeyboardEvent(E.page, event);
    });
}

void lzw_engine_focus(int focused) { onMain([focused] { WKViewSetFocus(E.view, focused != 0); }); }

} // extern "C"
