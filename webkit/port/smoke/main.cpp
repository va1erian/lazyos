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
//   WEBKIT:W4:PASS                        the title callback fired for the data URL and the page
//                                         painted ink (a page that never painted is a FAIL)
//
// The processes' paths come from the environment (LAZYOS_WEBPROCESS, LAZYOS_NETWORKPROCESS)
// or are the ones the image installs (LAZYOS_WEBPROCESS_PATH and LAZYOS_NETWORKPROCESS_PATH, set
// by the build from libs/fhs). argv[1] and argv[2] override the two URLs; with an argv[2] the
// title only has to be non-empty (the default page's title is "hello lazyos"). The watchdog gives
// up after WEBKIT_SMOKE_TIMEOUT seconds (default 90).
//
// Rules mode (issue #845, tools/webkit/blockcheck.py): with WEBKIT_SMOKE_RULES=<file.json> the
// program first compiles that content-rule list through WKUserContentExtensionStore (into
// WEBKIT_SMOKE_RULES_STORE, default a directory of $TMPDIR), looks it up again as a start does,
// attaches it to the page and only then loads argv[1]. It prints
//
//   WEBKIT:BLOCK:FILE:<bytes>                    the rule list
//   WEBKIT:BLOCK:COMPILE:<status>:<ms>:hwm_kb=<n>  WKUserContentExtensionStoreResult, time, peak RSS
//   WEBKIT:BLOCK:LOOKUP:<status>:<ms>            the same list read back from the store
//   WEBKIT:BLOCK:BLOCKED:<n>:NOTIFY:<m>          loads a rule blocked, notifications in all
//
// and exits non-zero when the list does not compile. Without the variable nothing changes.

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
#include <WebKit/WKArray.h>
#include <WebKit/WKType.h>
#include <WebKit/WKURL.h>
#include <WebKit/WKUserContentControllerRef.h>
#include <WebKit/WKUserContentExtensionStoreRef.h>
#include <WebKit/WKView.h>
#include <WebKit/WKViewClient.h>
#include <WebKit/WKWebsiteDataStoreRef.h>

#include <fontconfig/fontconfig.h>
#include <pthread.h>
#include <stdarg.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <string>
#include <time.h>
#include <unistd.h>
#include <vector>

namespace {

constexpr int kWidth = 800;
constexpr int kHeight = 600;
constexpr unsigned kDefaultTimeoutSeconds = 90;
constexpr unsigned kSettleMilliseconds = 1500;
constexpr const char* kDefaultTitle = "hello lazyos";

// The build sets these from libs/fhs (tools/webkit/port_cmake.py, WebKit/PlatformLazyOS.cmake).
#ifndef LAZYOS_WEBPROCESS_PATH
#error "LAZYOS_WEBPROCESS_PATH must name the image's webprocess (libs/fhs)"
#endif
#ifndef LAZYOS_NETWORKPROCESS_PATH
#error "LAZYOS_NETWORKPROCESS_PATH must name the image's networkprocess (libs/fhs)"
#endif

struct State {
    WKViewRef view { nullptr };
    WKPageRef page { nullptr };
    WKContextRef context { nullptr };
    std::string firstUrl { "about:blank" };
    std::string secondUrl { "data:text/html,<title>hello lazyos</title><p>x</p>" };
    int loadsFinished { 0 };
    int displayCallbacks { 0 };
    int titlePolls { 0 };
    long upMs { 0 }, aboutMs { 0 }, dataMs { 0 }, titleMs { 0 };
    bool titleSeen { false };
    bool customPage { false }; // argv[2] named the page: any title will do
    unsigned timeoutSeconds { kDefaultTimeoutSeconds };
    const char* stage { "init" };
    // Rules mode.
    std::string rulesFile;
    WKUserContentExtensionStoreRef store { nullptr };
    WKUserContentControllerRef controller { nullptr };
    long rulesStartMs { 0 };
    int blocked { 0 };
    int notifications { 0 };
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

// Milliseconds since the program started (CLOCK_MONOTONIC).
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
    sleep(state.timeoutSeconds);
    say("WEBKIT:W4:FAIL:%s:timeout after %u s", state.stage, state.timeoutSeconds);
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

// The process's peak resident set, in KiB (VmHWM), or 0.
long peakKiB()
{
    FILE* status = fopen("/proc/self/status", "r");
    if (!status)
        return 0;
    char line[256];
    long kib = 0;
    while (fgets(line, sizeof line, status)) {
        if (!strncmp(line, "VmHWM:", 6)) {
            kib = strtol(line + 6, nullptr, 10);
            break;
        }
    }
    fclose(status);
    return kib;
}

std::string readFile(const std::string& path)
{
    FILE* file = fopen(path.c_str(), "rb");
    if (!file)
        return std::string();
    std::string text;
    char buffer[65536];
    size_t n;
    while ((n = fread(buffer, 1, sizeof buffer, file)) > 0)
        text.append(buffer, n);
    fclose(file);
    return text;
}

// A lookup finished: the page gets the list, and loads.
void rulesLookedUp(WKUserContentFilterRef filter, WKUserContentExtensionStoreResult result, void*)
{
    say("WEBKIT:BLOCK:LOOKUP:%u:%ld", result, elapsedMs() - state.rulesStartMs);
    if (result != kWKUserContentExtensionStoreSuccess || !filter)
        fail("rules", "the stored list cannot be read back");
    WKUserContentControllerAddUserContentFilter(state.controller, filter);
    loadUrl(state.firstUrl);
}

void rulesCompiled(WKUserContentFilterRef filter, WKUserContentExtensionStoreResult result, void*)
{
    say("WEBKIT:BLOCK:COMPILE:%u:%ld:hwm_kb=%ld", result, elapsedMs() - state.rulesStartMs, peakKiB());
    if (result != kWKUserContentExtensionStoreSuccess || !filter)
        fail("rules", "WebKit refused the rule list");
    // The compiled list is not attached: a start reads it back from the store, so that is tested.
    WKRelease(filter);
    state.rulesStartMs = elapsedMs();
    WKStringRef identifier = makeString("smoke");
    WKUserContentExtensionStoreLookup(state.store, identifier, nullptr, rulesLookedUp);
    WKRelease(identifier);
}

void startRules()
{
    state.stage = "rules";
    std::string json = readFile(state.rulesFile);
    if (json.empty())
        fail("rules", "the rule list cannot be read");
    say("WEBKIT:BLOCK:FILE:%zu", json.size());
    state.rulesStartMs = elapsedMs();
    WKStringRef identifier = makeString("smoke");
    WKStringRef source = makeString(json);
    WKUserContentExtensionStoreCompile(state.store, identifier, source, nullptr, rulesCompiled);
    WKRelease(source);
    WKRelease(identifier);
}

void contentRuleListNotification(WKPageRef, WKURLRef, WKArrayRef, WKArrayRef notifications, const void*)
{
    for (size_t i = 0, n = notifications ? WKArrayGetSize(notifications) : 0; i < n; ++i) {
        ++state.notifications;
        WKTypeRef item = WKArrayGetItemAtIndex(notifications, i);
        if (item && WKGetTypeID(item) == WKStringGetTypeID()
            && WKStringIsEqualToUTF8CString(static_cast<WKStringRef>(item), "lazyos:blocked"))
            ++state.blocked;
    }
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
    // "Ink" is any pixel that is not the page's white; its bounding box shows where the text went.
    size_t drawn = 0, ink = 0;
    int x0 = kWidth, y0 = kHeight, x1 = -1, y1 = -1;
    for (int y = 0; y < kHeight; ++y) {
        for (int x = 0; x < kWidth; ++x) {
            const unsigned char* p = &pixels[(static_cast<size_t>(y) * kWidth + x) * 4];
            drawn += p[0] || p[1] || p[2] || p[3];
            if ((p[0] & p[1] & p[2] & p[3]) != 0xff) {
                ++ink;
                x0 = x < x0 ? x : x0;
                y0 = y < y0 ? y : y0;
                x1 = x > x1 ? x : x1;
                y1 = y > y1 ? y : y1;
            }
        }
    }
    // A page that never painted is not a pass: the whole point is that its pixels arrived.
    if (!drawn) {
        say("WEBKIT:W4:INFO:PAINT:pixels=0:displayCallbacks=%d", state.displayCallbacks);
        fail("paint", "nothing arrived");
    }
    if (!ink)
        fail("paint", "the page is blank (no ink)");
    {
        say("WEBKIT:W4:PAINT:%dx%d:%08x", kWidth, kHeight, crc32(pixels.data(), pixels.size()));
        say("WEBKIT:W4:INFO:PAINT:pixels=%zu:displayCallbacks=%d", drawn, state.displayCallbacks);
        say("WEBKIT:W4:INFO:INK:pixels=%zu:bbox=%d,%d,%d,%d", ink, x0, y0, x1, y1);
        // The ink as text (up to 40 x 96 pixels), so a run's glyphs can be looked at in the log.
        for (int y = y0; ink && y <= y1 && y < y0 + 96; ++y) {
            char row[48];
            int n = 0;
            for (int x = x0; x <= x1 && x < x0 + 40; ++x) {
                const unsigned char* p = &pixels[(static_cast<size_t>(y) * kWidth + x) * 4];
                int level = 255 - (p[0] + p[1] + p[2]) / 3;
                row[n++] = level > 192 ? '#' : level > 96 ? '+' : level > 16 ? '.' : ' ';
            }
            row[n] = 0;
            say("WEBKIT:W4:INFO:ART:%s|", row);
        }
    }
    say("WEBKIT:W4:INFO:TIME:up=%ld:about=%ld:data=%ld:title=%ld:paint=%ld", state.upMs, state.aboutMs, state.dataMs,
        state.titleMs, elapsedMs());
    if (!state.rulesFile.empty())
        say("WEBKIT:BLOCK:BLOCKED:%d:NOTIFY:%d", state.blocked, state.notifications);
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

std::string currentTitle()
{
    WKStringRef title = WKPageCopyTitle(state.page);
    std::string text = utf8(title);
    if (title)
        WKRelease(title);
    return text;
}

void startSettleThread()
{
    state.stage = "settle";
    pthread_t thread;
    if (pthread_create(&thread, nullptr, settleThenPaint, nullptr) != 0)
        fail("settle", "cannot start a thread");
    pthread_detach(thread);
}

// The title is the WebProcess's to report and can arrive a little after the load finished:
// look every 250 ms for up to ten seconds.
void checkTitle(void*);

void* retitle(void*)
{
    usleep(250 * 1000);
    WKRunLoopCallOnMainThread(checkTitle, nullptr);
    return nullptr;
}

void checkTitle(void*)
{
    std::string text = currentTitle();
    if (state.customPage ? !text.empty() : text == kDefaultTitle) {
        state.titleMs = elapsedMs();
        say("WEBKIT:W4:TITLE:%s", text.c_str());
        state.titleSeen = true;
        startSettleThread();
        return;
    }
    if (++state.titlePolls > 40) {
        say("WEBKIT:W4:TITLE:%s", text.c_str());
        fail("title", state.customPage ? "the page has no title" : "the title is not the one the page set");
    }
    pthread_t thread;
    if (pthread_create(&thread, nullptr, retitle, nullptr) != 0)
        fail("title", "cannot start a thread");
    pthread_detach(thread);
}

void didFinishNavigation(WKPageRef, WKNavigationRef, WKTypeRef, const void*)
{
    ++state.loadsFinished;
    if (state.loadsFinished == 1) {
        state.aboutMs = elapsedMs();
        say("WEBKIT:W4:LOAD:%s", state.firstUrl.c_str());
        loadUrl(state.secondUrl);
        return;
    }
    state.dataMs = elapsedMs();
    say("WEBKIT:W4:LOAD:%s", state.secondUrl.c_str());
    reportProcesses();
    state.stage = "title";
    checkTitle(nullptr);
}

void didFailNavigation(WKPageRef, WKNavigationRef, WKErrorRef, WKTypeRef, const void*)
{
    fail(state.stage, "the navigation failed");
}

void webProcessDidCrash(WKPageRef, const void*) { fail(state.stage, "the WebProcess crashed"); }

void viewNeedsDisplay(WKViewRef, WKRect, const void*) { ++state.displayCallbacks; }

// What Fontconfig (the font manager under WebKit's text) sees: how many fonts, and what the
// generic and common family names resolve to. A page with the wrong face is a font setup problem
// that this line shows at once.
void reportFonts()
{
    FcConfig* config = FcInitLoadConfigAndFonts();
    if (!config) {
        say("WEBKIT:W4:INFO:FONTS:no fontconfig configuration");
        return;
    }
    FcFontSet* set = FcConfigGetFonts(config, FcSetSystem);
    std::string line = "WEBKIT:W4:INFO:FONTS:count=" + std::to_string(set ? set->nfont : 0);
    for (const char* name : { "serif", "sans-serif", "monospace", "Times", "Arial" }) {
        FcPattern* pattern = FcNameParse(reinterpret_cast<const FcChar8*>(name));
        FcConfigSubstitute(config, pattern, FcMatchPattern);
        FcDefaultSubstitute(pattern);
        FcResult result;
        FcPattern* match = FcFontMatch(config, pattern, &result);
        FcChar8* family = nullptr;
        if (match)
            FcPatternGetString(match, FC_FAMILY, 0, &family);
        line += std::string(":") + name + "=" + (family ? reinterpret_cast<const char*>(family) : "?");
        if (match)
            FcPatternDestroy(match);
        FcPatternDestroy(pattern);
    }
    say("%s", line.c_str());
    FcConfigDestroy(config);
}

std::string environment(const char* name, const char* fallback)
{
    const char* value = getenv(name);
    return value && *value ? value : fallback;
}

// WEBKIT_SMOKE_TIMEOUT seconds, when it is a positive number.
unsigned timeoutFromEnvironment()
{
    const char* text = getenv("WEBKIT_SMOKE_TIMEOUT");
    char* end = nullptr;
    unsigned long seconds = text && *text ? strtoul(text, &end, 10) : 0;
    return seconds > 0 && end && !*end && seconds < 86400 ? static_cast<unsigned>(seconds) : kDefaultTimeoutSeconds;
}

} // namespace

int main(int argc, char** argv)
{
    setvbuf(stdout, nullptr, _IOLBF, 0);
    if (argc > 1)
        state.firstUrl = argv[1];
    if (argc > 2) {
        state.secondUrl = argv[2];
        state.customPage = true;
    }
    state.timeoutSeconds = timeoutFromEnvironment();

    pthread_t guard;
    pthread_create(&guard, nullptr, watchdog, nullptr);
    pthread_detach(guard);

    elapsedMs();
    WKRunLoopInitializeMain();
    state.upMs = elapsedMs();
    say("WEBKIT:W4:UP");
    reportFonts();

    state.stage = "context";
    WKContextConfigurationRef configuration = WKContextConfigurationCreate();
    WKStringRef webPath = makeString(environment("LAZYOS_WEBPROCESS", LAZYOS_WEBPROCESS_PATH));
    WKStringRef networkPath = makeString(environment("LAZYOS_NETWORKPROCESS", LAZYOS_NETWORKPROCESS_PATH));
    WKContextConfigurationSetWebProcessPath(configuration, webPath);
    WKContextConfigurationSetNetworkProcessPath(configuration, networkPath);
    state.context = WKContextCreateWithConfiguration(configuration);

    state.stage = "page";
    WKPageConfigurationRef pageConfiguration = WKPageConfigurationCreate();
    WKPageConfigurationSetContext(pageConfiguration, state.context);
    state.rulesFile = environment("WEBKIT_SMOKE_RULES", "");
    if (!state.rulesFile.empty()) {
        // The controller is part of the page's configuration, so it exists before the view.
        state.controller = WKUserContentControllerCreate();
        WKPageConfigurationSetUserContentController(pageConfiguration, state.controller);
        std::string folder = environment("WEBKIT_SMOKE_RULES_STORE",
            (environment("TMPDIR", P_tmpdir) + "/webkit-smoke-rules-" + std::to_string(getpid())).c_str());
        WKStringRef path = makeString(folder);
        state.store = WKUserContentExtensionStoreCreate(path);
        WKRelease(path);
        if (!state.store)
            fail("rules", "this WebKit has no content extensions");
    }
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

    // Version 3: the callback for what rule lists did exists from version 2 on.
    WKPageNavigationClientV3 navigation = { };
    navigation.base.version = 3;
    navigation.didFinishNavigation = didFinishNavigation;
    navigation.contentRuleListNotification = contentRuleListNotification;
    navigation.didFailNavigation = didFailNavigation;
    navigation.didFailProvisionalNavigation = [](WKPageRef, WKNavigationRef, WKErrorRef, WKTypeRef, const void*) {
        fail(state.stage, "the provisional navigation failed");
    };
    navigation.webProcessDidCrash = webProcessDidCrash;
    WKPageSetPageNavigationClient(state.page, &navigation.base);

    if (state.rulesFile.empty())
        loadUrl(state.firstUrl);
    else
        startRules();
    WKRunLoopRunMain();
    return state.titleSeen ? 0 : 1;
}
