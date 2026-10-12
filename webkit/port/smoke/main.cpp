// webkit-smoke: a headless WebKit UI process for LazyOS (docs/webkit-port-plan.md, W4).
//
// It does what a browser's UI process does with WebKit's C API and nothing else:
// creates a context pointed at the WebProcess and NetworkProcess binaries, a page
// configuration, a view (the PlayStation port's WKView, the non-WPE-backend path),
// loads about:blank, then a data: URL, waits for the title, and paints the page
// into a bitmap and checksums it. Every step prints one line the judge reads:
//
//   WEBKIT:W4:UP                          the UI process started and WebKit2 initialised
//   WEBKIT:W4:WEBPROCESS:UP:<pid>         a WebProcess was launched and answered
//   WEBKIT:W4:NETWORKPROCESS:UP:<pid>     a NetworkProcess was launched
//   WEBKIT:W4:LOAD:<url>                  the navigation to <url> finished
//   WEBKIT:W4:TITLE:<title>               the page's title, from the WebProcess
//   WEBKIT:W4:PAINT:<w>x<h>:<crc32>       the painted bitmap (and INFO lines on how much of it)
//   WEBKIT:W4:FAIL:<stage>:<reason>       the first thing that went wrong, then exit status 1
//   WEBKIT:W4:PASS                        the title callback fired for the data URL
//
// The processes' paths come from the environment (LAZYOS_WEBPROCESS, LAZYOS_NETWORKPROCESS)
// or are the ones the image installs; argv[1] and argv[2] override the two URLs.

#include <WebKit/WKBase.h>
#include <WebKit/WKContext.h>
#include <WebKit/WKContextConfigurationPlayStation.h>
#include <WebKit/WKContextConfigurationRef.h>
#include <WebKit/WKGeometry.h>
#include <WebKit/WKPage.h>
#include <WebKit/WKPageConfigurationRef.h>
#include <WebKit/WKPageNavigationClient.h>
#include <WebKit/WKPagePrivate.h>
#include <WebKit/WKPagePrivatePlayStation.h>
#include <WebKit/WKRunLoop.h>
#include <WebKit/WKString.h>
#include <WebKit/WKURL.h>
#include <WebKit/WKView.h>
#include <WebKit/WKViewClient.h>
#include <WebKit/WKWebsiteDataStoreRef.h>

#include <pthread.h>
#include <stdarg.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <string>
#include <unistd.h>
#include <vector>

namespace {

constexpr int kWidth = 800;
constexpr int kHeight = 600;
constexpr unsigned kTimeoutSeconds = 90;
constexpr unsigned kSettleMilliseconds = 1500;
constexpr const char* kDefaultTitle = "hello lazyos";

struct State {
    WKViewRef view { nullptr };
    WKPageRef page { nullptr };
    WKContextRef context { nullptr };
    std::string firstUrl { "about:blank" };
    std::string secondUrl { "data:text/html,<title>hello lazyos</title><p>x</p>" };
    int loadsFinished { 0 };
    int displayCallbacks { 0 };
    bool titleSeen { false };
    const char* stage { "init" };
} state;

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

[[noreturn]] void fail(const char* stage, const char* reason)
{
    say("WEBKIT:W4:FAIL:%s:%s", stage, reason);
    fflush(stdout);
    _exit(1);
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

WKStringRef makeString(const std::string& text) { return WKStringCreateWithUTF8CString(text.c_str()); }

uint32_t crc32(const unsigned char* data, size_t size)
{
    uint32_t table[256];
    for (uint32_t i = 0; i < 256; ++i) {
        uint32_t c = i;
        for (int k = 0; k < 8; ++k)
            c = (c & 1) ? 0xedb88320u ^ (c >> 1) : c >> 1;
        table[i] = c;
    }
    uint32_t crc = 0xffffffffu;
    for (size_t i = 0; i < size; ++i)
        crc = table[(crc ^ data[i]) & 0xff] ^ (crc >> 8);
    return ~crc;
}

// A thread that gives up: a hung WebProcess must not hang the harness.
void* watchdog(void*)
{
    sleep(kTimeoutSeconds);
    say("WEBKIT:W4:FAIL:%s:timeout after %u s", state.stage, kTimeoutSeconds);
    _exit(1);
    return nullptr;
}

void loadUrl(const std::string& url)
{
    state.stage = "load";
    WKURLRef wkUrl = WKURLCreateWithUTF8CString(url.c_str());
    WKPageLoadURL(state.page, wkUrl);
    WKRelease(wkUrl);
}

void reportProcesses()
{
    pid_t web = WKPageGetProcessIdentifier(state.page);
    if (web > 0 && kill(web, 0) == 0)
        say("WEBKIT:W4:WEBPROCESS:UP:%d", static_cast<int>(web));
    else
        fail("webprocess", "no WebProcess is running");
    WKWebsiteDataStoreRef store = WKContextGetWebsiteDataStore(state.context);
    pid_t network = store ? WKWebsiteDataStoreGetNetworkProcessIdentifier(store) : 0;
    if (network > 0 && kill(network, 0) == 0)
        say("WEBKIT:W4:NETWORKPROCESS:UP:%d", static_cast<int>(network));
    else
        fail("networkprocess", "no NetworkProcess is running");
}

void paintAndFinish(void*)
{
    state.stage = "paint";
    std::vector<unsigned char> pixels(static_cast<size_t>(kWidth) * kHeight * 4, 0);
    WKPagePaint(state.page, pixels.data(), WKSizeMake(kWidth, kHeight), WKRectMake(0, 0, kWidth, kHeight));
    size_t drawn = 0;
    for (size_t i = 0; i < pixels.size(); i += 4)
        drawn += pixels[i] || pixels[i + 1] || pixels[i + 2] || pixels[i + 3];
    if (drawn) {
        say("WEBKIT:W4:PAINT:%dx%d:%08x", kWidth, kHeight, crc32(pixels.data(), pixels.size()));
        say("WEBKIT:W4:INFO:PAINT:pixels=%zu:displayCallbacks=%d", drawn, state.displayCallbacks);
    } else
        say("WEBKIT:W4:INFO:PAINT:nothing arrived (displayCallbacks=%d)", state.displayCallbacks);
    say("WEBKIT:W4:PASS");
    WKRunLoopStopMain();
}

// The page has a title: give the first frame a moment to arrive, then paint.
void* settleThenPaint(void*)
{
    usleep(kSettleMilliseconds * 1000);
    WKRunLoopCallOnMainThread(paintAndFinish, nullptr);
    return nullptr;
}

void didFinishNavigation(WKPageRef, WKNavigationRef, WKTypeRef, const void*)
{
    ++state.loadsFinished;
    if (state.loadsFinished == 1) {
        say("WEBKIT:W4:LOAD:%s", state.firstUrl.c_str());
        loadUrl(state.secondUrl);
        return;
    }
    say("WEBKIT:W4:LOAD:%s", state.secondUrl.c_str());
    WKStringRef title = WKPageCopyTitle(state.page);
    std::string text = utf8(title);
    if (title)
        WKRelease(title);
    say("WEBKIT:W4:TITLE:%s", text.c_str());
    if (text != kDefaultTitle)
        fail("title", "the title is not the one the page set");
    state.titleSeen = true;
    reportProcesses();
    state.stage = "settle";
    pthread_t thread;
    if (pthread_create(&thread, nullptr, settleThenPaint, nullptr) != 0)
        fail("settle", "cannot start a thread");
    pthread_detach(thread);
}

void didFailNavigation(WKPageRef, WKNavigationRef, WKErrorRef, WKTypeRef, const void*)
{
    fail(state.stage, "the navigation failed");
}

void webProcessDidCrash(WKPageRef, const void*) { fail(state.stage, "the WebProcess crashed"); }

void viewNeedsDisplay(WKViewRef, WKRect, const void*) { ++state.displayCallbacks; }

std::string environment(const char* name, const char* fallback)
{
    const char* value = getenv(name);
    return value && *value ? value : fallback;
}

} // namespace

int main(int argc, char** argv)
{
    setvbuf(stdout, nullptr, _IOLBF, 0);
    if (argc > 1)
        state.firstUrl = argv[1];
    if (argc > 2)
        state.secondUrl = argv[2];

    pthread_t guard;
    pthread_create(&guard, nullptr, watchdog, nullptr);
    pthread_detach(guard);

    WKRunLoopInitializeMain();
    say("WEBKIT:W4:UP");

    state.stage = "context";
    WKContextConfigurationRef configuration = WKContextConfigurationCreate();
    WKStringRef webPath = makeString(environment("LAZYOS_WEBPROCESS", "/system/bin/WebProcess"));
    WKStringRef networkPath = makeString(environment("LAZYOS_NETWORKPROCESS", "/system/bin/NetworkProcess"));
    WKContextConfigurationSetWebProcessPath(configuration, webPath);
    WKContextConfigurationSetNetworkProcessPath(configuration, networkPath);
    state.context = WKContextCreateWithConfiguration(configuration);

    state.stage = "page";
    WKPageConfigurationRef pageConfiguration = WKPageConfigurationCreate();
    WKPageConfigurationSetContext(pageConfiguration, state.context);
    state.view = WKViewCreate(pageConfiguration);
    if (!state.view)
        fail("page", "WKViewCreate returned null");
    state.page = WKViewGetPage(state.view);

    WKViewClientV0 viewClient = { };
    viewClient.base.version = 0;
    viewClient.setViewNeedsDisplay = viewNeedsDisplay;
    WKViewSetViewClient(state.view, &viewClient.base);
    WKViewSetSize(state.view, WKSizeMake(kWidth, kHeight));
    WKViewSetActive(state.view, true);
    WKViewSetFocus(state.view, true);
    WKViewSetVisible(state.view, true);

    WKPageNavigationClientV0 navigation = { };
    navigation.base.version = 0;
    navigation.didFinishNavigation = didFinishNavigation;
    navigation.didFailNavigation = didFailNavigation;
    navigation.didFailProvisionalNavigation = [](WKPageRef, WKNavigationRef, WKErrorRef, WKTypeRef, const void*) {
        fail(state.stage, "the provisional navigation failed");
    };
    navigation.webProcessDidCrash = webProcessDidCrash;
    WKPageSetPageNavigationClient(state.page, &navigation.base);

    loadUrl(state.firstUrl);
    WKRunLoopRunMain();
    return state.titleSeen ? 0 : 1;
}
