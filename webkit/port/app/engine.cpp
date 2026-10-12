// WebKit's UI-process half for the desktop window (docs/webkit-port-plan.md, W5).
//
// Everything here runs on WebKit's main thread (the run loop main() starts). The window's
// thread calls the lzw_engine_* functions, each of which only queues a task for this
// thread (lazyos_ui.h). WebKit is used through its C API alone, as webkit-smoke does:
// a context pointed at the WebProcess and NetworkProcess binaries, a page configuration,
// the PlayStation port's WKView; the page is painted into a bitmap with WKPagePaint and
// handed to the window; mouse, wheel and key events go in through WKPageHandle*Event.
//
// Markers (one line each, on stdout; tools/webkit/appjudge.py reads them):
//   WEBKIT:W5:WEBPROCESS:UP:<pid>       the page's WebProcess is running (after the first load)
//   WEBKIT:W5:NETWORKPROCESS:UP:<pid>   the NetworkProcess is running
//   WEBKIT:W5:LOAD:<url>                a navigation finished (the page's own URL)
//   WEBKIT:W5:TITLE:<title>             the title when it finished, and again when script changes it
//   WEBKIT:W5:JS:<title>                document.title, read back by running script in the page
//   WEBKIT:W5:CLICK:<url>               a link was clicked: the navigation it started
//   WEBKIT:W5:INPUT:<title>             a form was submitted and the response's title (the echo)
//   WEBKIT:W5:FAIL:<what>               a navigation failed or the WebProcess crashed
//   WEBKIT:W5:INFO:TIME:...             milliseconds: start, load start/finish, first paint
//   WEBKIT:W5:INFO:MEM:...              what /proc/meminfo says about the guest (when it can be read)

#include <WebKit/WKBase.h>
#include <WebKit/WKContext.h>
#include <WebKit/WKContextConfigurationPlayStation.h>
#include <WebKit/WKContextConfigurationRef.h>
#include <WebKit/WKData.h>
#include <WebKit/WKErrorRef.h>
#include <WebKit/WKFramePolicyListener.h>
#include <WebKit/WKGeometry.h>
#include <WebKit/WKNavigationActionRef.h>
#include <WebKit/WKPage.h>
#include <WebKit/WKPageConfigurationRef.h>
#include <WebKit/WKPageNavigationClient.h>
#include <WebKit/WKPagePrivate.h>
#include <WebKit/WKPagePrivatePlayStation.h>
#include <WebKit/WKPageStateClient.h>
#include <WebKit/WKRunLoop.h>
#include <WebKit/WKString.h>
#include <WebKit/WKType.h>
#include <WebKit/WKURL.h>
#include <WebKit/WKURLRequest.h>
#include <WebKit/WKView.h>
#include <WebKit/WKViewClient.h>
#include <WebKit/WKWebsiteDataStoreRef.h>

#include <functional>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <string>
#include <time.h>
#include <unistd.h>
#include <vector>

#include "lazyos_ui.h"

namespace {

constexpr int kInitialWidth = 1000;
constexpr int kInitialHeight = 500;

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
    bool committedSincePaint { false };
    bool loadFinished { false };
    std::string lastTitleSaid;
    long navStartMs { 0 };
    long commitMs { 0 };
    long firstPaintMs { -1 };
    int navigations { 0 };
} E;

void say(const char* format, ...) __attribute__((format(printf, 1, 2)));
void say(const char* format, ...)
{
    va_list args;
    va_start(args, format);
    vprintf(format, args);
    va_end(args);
    printf("\n");
    fflush(stdout);
}

long elapsedMs()
{
    static const long origin = [] {
        timespec now;
        clock_gettime(CLOCK_MONOTONIC, &now);
        return now.tv_sec * 1000L + now.tv_nsec / 1000000L;
    }();
    timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec * 1000L + now.tv_nsec / 1000000L - origin;
}

std::string utf8(WKStringRef string)
{
    if (!string)
        return std::string();
    size_t size = WKStringGetMaximumUTF8CStringSize(string);
    std::vector<char> buffer(size + 1);
    size_t written = WKStringGetUTF8CString(string, buffer.data(), buffer.size());
    return std::string(buffer.data(), written ? written - 1 : 0);
}

std::string utf8(WKURLRef url)
{
    if (!url)
        return std::string();
    WKStringRef string = WKURLCopyString(url);
    std::string text = utf8(string);
    if (string)
        WKRelease(string);
    return text;
}

// A line of text for a marker: no newlines, so one marker stays one line.
std::string oneLine(std::string text)
{
    for (char& c : text) {
        if (c == '\n' || c == '\r')
            c = ' ';
    }
    return text;
}

std::string activeUrl()
{
    WKURLRef url = WKPageCopyActiveURL(E.page);
    std::string text = utf8(url);
    if (url)
        WKRelease(url);
    return text;
}

std::string pageTitle()
{
    WKStringRef title = WKPageCopyTitle(E.page);
    std::string text = utf8(title);
    if (title)
        WKRelease(title);
    return text;
}

// Tasks from other threads: queued on the run loop, run once, freed.
void runTask(void* context)
{
    auto* task = static_cast<std::function<void()>*>(context);
    (*task)();
    delete task;
}

void onMain(std::function<void()> task)
{
    WKRunLoopCallOnMainThread(runTask, new std::function<void()>(std::move(task)));
}

// ---- state for the window -------------------------------------------------------------

void postState()
{
    std::string url = activeUrl();
    std::string title = pageTitle();
    int progress = static_cast<int>(WKPageGetEstimatedProgress(E.page) * 1000.0);
    lzw_ui_state(url.c_str(), title.c_str(), E.loading ? 1 : 0, WKPageCanGoBack(E.page) ? 1 : 0,
        WKPageCanGoForward(E.page) ? 1 : 0, E.loading ? progress : 1000);
}

// ---- painting -------------------------------------------------------------------------

void paintNow(void*)
{
    E.paintScheduled = false;
    if (!E.page || E.width <= 0 || E.height <= 0)
        return;
    const size_t bytes = static_cast<size_t>(E.width) * E.height * 4;
    if (E.pixels.size() != bytes)
        E.pixels.assign(bytes, 0xff);
    // What the page does not cover is white, never see-through.
    memset(E.pixels.data(), 0xff, bytes);
    WKPagePaint(E.page, E.pixels.data(), WKSizeMake(E.width, E.height), WKRectMake(0, 0, E.width, E.height));
    // Skia's N32 is BGRA on this target and premultiplied; the window wants opaque RGBA.
    uint8_t* p = E.pixels.data();
    for (size_t i = 0; i < bytes; i += 4) {
        uint8_t blue = p[i];
        p[i] = p[i + 2];
        p[i + 2] = blue;
        p[i + 3] = 0xff;
    }
    lzw_ui_frame(E.pixels.data(), E.width, E.height);
    if (E.firstPaintMs < 0 && E.committedSincePaint) {
        E.firstPaintMs = elapsedMs();
        say("WEBKIT:W5:INFO:TIME:firstpaint=%ld:sincecommit=%ld:sincenav=%ld", E.firstPaintMs, E.firstPaintMs - E.commitMs,
            E.firstPaintMs - E.navStartMs);
    }
}

// Many rectangles arrive for one change: one paint takes them all.
void schedulePaint()
{
    if (E.paintScheduled)
        return;
    E.paintScheduled = true;
    WKRunLoopCallOnMainThread(paintNow, nullptr);
}

void viewNeedsDisplay(WKViewRef, WKRect, const void*) { schedulePaint(); }

// ---- the page's news ------------------------------------------------------------------

void reportProcessesOnce()
{
    if (E.processesReported)
        return;
    E.processesReported = true;
    pid_t web = WKPageGetProcessIdentifier(E.page);
    if (web > 0)
        say("WEBKIT:W5:WEBPROCESS:UP:%d", static_cast<int>(web));
    WKWebsiteDataStoreRef store = WKContextGetWebsiteDataStore(E.context);
    pid_t network = store ? WKWebsiteDataStoreGetNetworkProcessIdentifier(store) : 0;
    if (network > 0)
        say("WEBKIT:W5:NETWORKPROCESS:UP:%d", static_cast<int>(network));
}

void reportMemory()
{
    FILE* file = fopen("/proc/meminfo", "r");
    if (!file)
        return;
    char line[160];
    std::string text;
    while (fgets(line, sizeof line, file)) {
        if (!strncmp(line, "MemTotal", 8) || !strncmp(line, "MemFree", 7) || !strncmp(line, "MemAvailable", 12)) {
            std::string field = oneLine(line);
            while (!field.empty() && (field.back() == ' '))
                field.pop_back();
            text += (text.empty() ? "" : ":") + field;
        }
    }
    fclose(file);
    if (!text.empty())
        say("WEBKIT:W5:INFO:MEM:%s", text.c_str());
}

void sayTitle()
{
    std::string title = oneLine(pageTitle());
    if (title.empty())
        return;
    say("WEBKIT:W5:TITLE:%s", title.c_str());
    E.lastTitleSaid = title;
}

// Reads document.title back by running script in the page: the proof that script ran there.
void scriptResult(WKTypeRef result, WKErrorRef, void*)
{
    std::string title;
    if (result && WKGetTypeID(result) == WKStringGetTypeID())
        title = utf8(static_cast<WKStringRef>(result));
    say("WEBKIT:W5:JS:%s", oneLine(title).c_str());
}

void readTitleBySript()
{
    WKStringRef script = WKStringCreateWithUTF8CString("document.title");
    WKPageEvaluateJavaScriptInMainFrame(E.page, script, nullptr, scriptResult);
    WKRelease(script);
}

void decidePolicyForNavigationAction(WKPageRef, WKNavigationActionRef action, WKFramePolicyListenerRef listener, WKTypeRef, const void*)
{
    WKFrameNavigationType type = WKNavigationActionGetNavigationType(action);
    if (type == kWKFrameNavigationTypeLinkClicked || type == kWKFrameNavigationTypeFormSubmitted) {
        WKURLRequestRef request = WKNavigationActionCopyRequest(action);
        WKURLRef url = request ? WKURLRequestCopyURL(request) : nullptr;
        if (type == kWKFrameNavigationTypeLinkClicked)
            say("WEBKIT:W5:CLICK:%s", oneLine(utf8(url)).c_str());
        else
            E.formNavigation = true;
        if (url)
            WKRelease(url);
        if (request)
            WKRelease(request);
    }
    WKFramePolicyListenerUse(listener);
}

void didStartProvisionalNavigation(WKPageRef, WKNavigationRef, WKTypeRef, const void*)
{
    E.loading = true;
    E.loadFinished = false;
    E.navStartMs = elapsedMs();
    E.committedSincePaint = false;
    E.firstPaintMs = -1;
    ++E.navigations;
    postState();
}

void didCommitNavigation(WKPageRef, WKNavigationRef, WKTypeRef, const void*)
{
    E.commitMs = elapsedMs();
    E.committedSincePaint = true;
    postState();
}

void didFinishNavigation(WKPageRef, WKNavigationRef, WKTypeRef, const void*)
{
    E.loading = false;
    E.loadFinished = true;
    long finished = elapsedMs();
    say("WEBKIT:W5:LOAD:%s", oneLine(activeUrl()).c_str());
    sayTitle();
    if (E.formNavigation) {
        E.formNavigation = false;
        say("WEBKIT:W5:INPUT:%s", oneLine(pageTitle()).c_str());
    }
    readTitleBySript();
    reportProcessesOnce();
    say("WEBKIT:W5:INFO:TIME:nav=%d:start=%ld:commit=%ld:finish=%ld:load=%ld", E.navigations, E.navStartMs, E.commitMs,
        finished, finished - E.navStartMs);
    reportMemory();
    postState();
    schedulePaint();
}

void didFail(const char* what, WKErrorRef error)
{
    E.loading = false;
    std::string description;
    if (error) {
        WKStringRef text = WKErrorCopyLocalizedDescription(error);
        description = utf8(text);
        if (text)
            WKRelease(text);
        description += " (code " + std::to_string(WKErrorGetErrorCode(error)) + ")";
    }
    say("WEBKIT:W5:FAIL:%s:%s:%s", what, oneLine(activeUrl()).c_str(), oneLine(description).c_str());
    postState();
}

void didFailNavigation(WKPageRef, WKNavigationRef, WKErrorRef error, WKTypeRef, const void*) { didFail("navigation", error); }

void didFailProvisionalNavigation(WKPageRef, WKNavigationRef, WKErrorRef error, WKTypeRef, const void*)
{
    didFail("provisional", error);
}

void webProcessDidCrash(WKPageRef, const void*)
{
    E.loading = false;
    say("WEBKIT:W5:FAIL:webprocess:crashed");
    postState();
}

void stateChanged(const void*) { postState(); }

// The title can change after the load finished (script): say so, once per new title.
void titleChanged(const void*)
{
    if (E.loadFinished && oneLine(pageTitle()) != E.lastTitleSaid)
        sayTitle();
    postState();
}

std::string environment(const char* name, const char* fallback)
{
    const char* value = getenv(name);
    return value && *value ? value : fallback;
}

} // namespace

extern "C" {

int lzw_engine_start(void)
{
    elapsedMs();
    WKContextConfigurationRef configuration = WKContextConfigurationCreate();
    WKStringRef webPath = WKStringCreateWithUTF8CString(environment("LAZYOS_WEBPROCESS", "/system/bin/webprocess").c_str());
    WKStringRef networkPath = WKStringCreateWithUTF8CString(environment("LAZYOS_NETWORKPROCESS", "/system/bin/networkprocess").c_str());
    WKContextConfigurationSetWebProcessPath(configuration, webPath);
    WKContextConfigurationSetNetworkProcessPath(configuration, networkPath);
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

    static WKPageNavigationClientV0 navigation = { };
    navigation.base.version = 0;
    navigation.decidePolicyForNavigationAction = decidePolicyForNavigationAction;
    navigation.didStartProvisionalNavigation = didStartProvisionalNavigation;
    navigation.didCommitNavigation = didCommitNavigation;
    navigation.didFinishNavigation = didFinishNavigation;
    navigation.didFailNavigation = didFailNavigation;
    navigation.didFailProvisionalNavigation = didFailProvisionalNavigation;
    navigation.webProcessDidCrash = webProcessDidCrash;
    WKPageSetPageNavigationClient(E.page, &navigation.base);

    static WKPageStateClientV0 state = { };
    state.base.version = 0;
    state.didChangeIsLoading = stateChanged;
    state.didChangeTitle = titleChanged;
    state.didChangeActiveURL = stateChanged;
    state.didChangeEstimatedProgress = stateChanged;
    state.didChangeCanGoBack = stateChanged;
    state.didChangeCanGoForward = stateChanged;
    WKPageSetPageStateClient(E.page, &state.base);
    return 0;
}

void lzw_engine_quit(void) { WKRunLoopCallOnMainThread([](void*) { WKRunLoopStopMain(); }, nullptr); }

void lzw_engine_load(const char* url)
{
    std::string address = url ? url : "";
    onMain([address] {
        WKURLRef wkUrl = WKURLCreateWithUTF8CString(address.c_str());
        say("WEBKIT:W5:NAV:%s", oneLine(address).c_str());
        WKPageLoadURL(E.page, wkUrl);
        WKRelease(wkUrl);
    });
}

void lzw_engine_reload(void) { onMain([] { WKPageReload(E.page); }); }
void lzw_engine_stop(void) { onMain([] { WKPageStopLoading(E.page); }); }
void lzw_engine_back(void) { onMain([] { WKPageGoBack(E.page); }); }
void lzw_engine_forward(void) { onMain([] { WKPageGoForward(E.page); }); }

void lzw_engine_resize(int width, int height)
{
    onMain([width, height] {
        if (width <= 0 || height <= 0 || (width == E.width && height == E.height))
            return;
        E.width = width;
        E.height = height;
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
