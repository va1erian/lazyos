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

#include <stdint.h>
#include <string>

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

} // namespace webkitapp

using namespace webkitapp;

extern "C" {

int lzw_engine_start(void)
{
    elapsedMs();
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
        for (int i = 0; i < (ticksX < 0 ? -ticksX : ticksX) && i < 8; ++i)
            WKPageHandleWheelEvent(E.page, WKWheelEventMake(kWKEventWheel, WKPointMake(x, y), WKSizeMake(ticksX < 0 ? -1 : 1, 0), WKSizeMake(0, 0), mods));
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
