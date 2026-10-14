// What the page tells the UI process: navigation and state clients, the markers a session
// reads, and the state handed to the window (docs/webkit-port-plan.md, W5; docs/lazywebplus.md).
//
// Markers (one line each, on stdout, after the window's prefix: WEBKIT:W5 or WEBPLUS;
// tools/webkit/appjudge.py and tools/webplus/judge.py read them):
//   WEBPROCESS:UP:<pid>       the page's WebProcess is running (printed again when its id changes)
//   NETWORKPROCESS:UP:<pid>   the NetworkProcess is running (a replacement prints its own)
//   WEBPROCESS:GONE:<pid>:<reason>    the page's process ended (crash, memory, cpu, requested)
//   WEBPROCESS:UNRESPONSIVE:<pid> / WEBPROCESS:RESPONSIVE:<pid>   WebKit's 6 s ping went unanswered, or
//                                                                 was answered again
//   NETWORKPROCESS:GONE:<pid>:<reason>  the NetworkProcess crashed, ran out of memory or time, or did
//                                       not answer and WebKit replaced it (WebKit's C API calls all of
//                                       those but the first three "crash")
// The ones that name a page (LOAD, TITLE, JS, CLICK, INPUT, and the URL in FAIL) print only in
// an image built with the UI probe (sayHarness), and a window that prints them itself
// (LZW_FEATURE_OWN_MARKERS) has none of them from here.
//   START:<url>               a navigation began (the page it is going to)
//   CANCELLED:<what>          a load ended because the user stopped it (not a failure)
//   LOAD:<url>                a navigation finished (the page's own URL)
//   TITLE:<title>             the title when it finished, and again when script changes it
//   JS:<title>                document.title, read back by running script in the page
//   CLICK:<url>               a link was clicked: the navigation it started
//   INPUT:<title>             a form was submitted and the response's title (the echo)
//   FAIL:<what>               a navigation failed or the WebProcess crashed
//   INFO:TIME:...             milliseconds: start, load start/finish, first paint
//   INFO:MEM:...              the guest's RAM in MiB (sysinfo): total, free, and what this
//                             process and its children took since the UI process started

#include <WebKit/WKContext.h>
#include <WebKit/WKProcessTerminationReason.h>
#include <WebKit/WKErrorRef.h>
#include <WebKit/WKFramePolicyListener.h>
#include <WebKit/WKNavigationActionRef.h>
#include <WebKit/WKPage.h>
#include <WebKit/WKPageNavigationClient.h>
#include <WebKit/WKPagePrivate.h>
#include <WebKit/WKPagePrivatePlayStation.h>
#include <WebKit/WKPageStateClient.h>
#include <WebKit/WKString.h>
#include <WebKit/WKType.h>
#include <WebKit/WKURL.h>
#include <WebKit/WKURLRequest.h>
#include <WebKit/WKWebsiteDataStoreRef.h>

#include <stdint.h>
#include <string>
#include <sys/sysinfo.h>
#include <unistd.h>

#include "engine.h"
#include "lazyos_ui.h"

namespace webkitapp {

std::string activeUrl()
{
    WKURLRef url = WKPageCopyActiveURL(E.page);
    std::string text = utf8(url);
    if (url)
        WKRelease(url);
    return text;
}

// Tells the window whether the page's process answers WebKit's pings, as WebKit says now (also when a
// process the window has not heard of appears, and when a page commits: a stale "not responding" must
// not make Reload or Stop end a healthy process).
static void reportResponsiveness()
{
    const bool responsive = WKPageIsWebProcessResponsive(E.page);
    E.unresponsive = !responsive;
    lzw_ui_page_responsive(responsive ? 1 : 0);
}

void rememberChildren()
{
    if (!E.page)
        return;
    // A process is announced when WebKit names an id the launcher has started and not seen end, and the
    // id is new or a launch has happened since the last announcement (the kernel gives the lowest free
    // slot, so a replacement often takes the dead process's id). An id the launcher does not hold is a
    // stale answer (WebKit names a process after it ended): not announced, not remembered.
    const int web = static_cast<int>(WKPageGetProcessIdentifier(E.page));
    const unsigned webLaunches = processLaunches("web");
    if (web > 0 && processLive("web", web) && (web != E.webPidShown || webLaunches != E.webLaunchSeen)) {
        E.webPid = web;
        E.webPidShown = web;
        E.webLaunchSeen = webLaunches;
        say("WEBPROCESS:UP:%d", web);
        // A process the window has not heard of: whatever it was told of the last one (not answering)
        // does not hold for this one.
        reportResponsiveness();
    }
    WKWebsiteDataStoreRef store = E.store ? E.store : WKContextGetWebsiteDataStore(E.context);
    const int network = store ? static_cast<int>(WKWebsiteDataStoreGetNetworkProcessIdentifier(store)) : 0;
    const unsigned networkLaunches = processLaunches("network");
    if (network > 0 && processLive("network", network)
        && (network != E.networkPidShown || networkLaunches != E.networkLaunchSeen)) {
        E.networkPid = network;
        E.networkPidShown = network;
        E.networkLaunchSeen = networkLaunches;
        say("NETWORKPROCESS:UP:%d", network);
    }
}

const char* terminationReason(uint32_t reason)
{
    switch (reason) {
    case kWKProcessTerminationReasonExceededMemoryLimit:
        return "memory";
    case kWKProcessTerminationReasonExceededCPULimit:
        return "cpu";
    case kWKProcessTerminationReasonRequestedByClient:
        return "requested";
    default:
        return "crash";
    }
}

std::string committedUrl()
{
    WKURLRef url = WKPageCopyCommittedURL(E.page);
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

// ---- state for the window -------------------------------------------------------------

void postState()
{
    // The WebProcess exists as soon as the page does: when did it come up? A process that replaced
    // another (a crash, a recovery) or a new NetworkProcess is noticed here as well.
    static bool webProcessSeen = false;
    if (!webProcessSeen && WKPageGetProcessIdentifier(E.page) > 0) {
        webProcessSeen = true;
        say("INFO:TIME:webprocess=%ld", elapsedMs());
    }
    rememberChildren();
    std::string url = activeUrl();
    std::string committed = committedUrl();
    std::string title = pageTitle();
    int progress = static_cast<int>(WKPageGetEstimatedProgress(E.page) * 1000.0);
    // A committed https: page was fetched over a verified connection: a certificate that does not
    // verify fails the load (the TLS plan's rule), it never shows a page.
    const bool secure = committed.size() > 8 && committed.compare(0, 8, "https://") == 0;
    lzw_ui_state(url.c_str(), title.c_str(), E.loading ? 1 : 0, WKPageCanGoBack(E.page) ? 1 : 0,
        WKPageCanGoForward(E.page) ? 1 : 0, E.loading ? progress : 1000, E.error.c_str(), E.crashed ? 1 : 0,
        committed.c_str(), secure ? 1 : 0, E.finished);
}

namespace {

// ---- the page's news ------------------------------------------------------------------

// The guest's RAM, in MiB, after each load: what three processes of tens of MB and a page cost.
void reportMemory()
{
    static long firstFreeMiB = -1;
    struct sysinfo info;
    if (sysinfo(&info) != 0)
        return;
    const long unit = info.mem_unit ? info.mem_unit : 1;
    const long totalMiB = static_cast<long>(info.totalram * unit / (1024 * 1024));
    const long freeMiB = static_cast<long>(info.freeram * unit / (1024 * 1024));
    if (firstFreeMiB < 0)
        firstFreeMiB = freeMiB;
    say("INFO:MEM:total=%ld:free=%ld:used_since_first_load=%ld", totalMiB, freeMiB, firstFreeMiB - freeMiB);
    uint64_t used = 0, limit = 0;
    if (!lzw_ui_user_memory(&used, &limit))
        say("INFO:QUOTA:user_memory_mib=%lu:limit_mib=%lu", static_cast<unsigned long>(used >> 20), static_cast<unsigned long>(limit >> 20));
}

void sayTitle()
{
    std::string title = oneLine(pageTitle());
    if (title.empty())
        return;
    sayHarness("TITLE:%s", title.c_str());
    E.lastTitleSaid = title;
}

// The title of the page a form submission led to: what the server echoed back.
void sayInput()
{
    if (!E.inputPending || !E.loadFinished)
        return;
    std::string title = oneLine(pageTitle());
    if (title.empty())
        return;
    E.inputPending = false;
    sayHarness("INPUT:%s", title.c_str());
}

// Reads document.title back by running script in the page: the proof that script ran there.
void scriptResult(WKTypeRef result, WKErrorRef, void*)
{
    std::string title;
    if (result && WKGetTypeID(result) == WKStringGetTypeID())
        title = utf8(static_cast<WKStringRef>(result));
    sayHarness("JS:%s", oneLine(title).c_str());
}

void readTitleBySript()
{
    // Only for a session: it is the proof that script ran in the page, and costs a round trip
    // to the WebProcess per page.
    if (!harness())
        return;
    WKStringRef script = WKStringCreateWithUTF8CString("document.title");
    WKPageEvaluateJavaScriptInMainFrame(E.page, script, nullptr, scriptResult);
    WKRelease(script);
}

void didStartProvisionalNavigation(WKPageRef, WKNavigationRef, WKTypeRef, const void*)
{
    dropDialogs();
    E.error.clear();
    E.crashed = false;
    E.stopRequested = false;
    E.loading = true;
    E.loadFinished = false;
    sayHarness("START:%s", oneLine(activeUrl()).c_str());
    E.navStartMs = elapsedMs();
    E.committedSincePaint = false;
    E.firstPaintMs = -1;
    ++E.navigations;
    postState();
}

void didCommitNavigation(WKPageRef, WKNavigationRef, WKTypeRef, const void*)
{
    E.saveNext = false; // a page was committed: whatever was to be saved is not this
    E.commitMs = elapsedMs();
    E.committedSincePaint = true;
    reportResponsiveness();
    // After redirects the committed address is the one that counts, and it is known before the page's
    // first subresource is asked for.
    blockingSettle();
    blockingCommitted();
    postState();
}

void didFinishNavigation(WKPageRef, WKNavigationRef, WKTypeRef, const void*)
{
    E.error.clear();
    E.loading = false;
    E.loadFinished = true;
    ++E.finished;
    long finished = elapsedMs();
    sayHarness("LOAD:%s", oneLine(activeUrl()).c_str());
    sayTitle();
    if (E.formNavigation) {
        // The echo is the response's title; it can arrive a moment after the load finished.
        E.formNavigation = false;
        E.inputPending = true;
        sayInput();
    }
    readTitleBySript();
    rememberChildren();
    say("INFO:TIME:nav=%d:start=%ld:commit=%ld:finish=%ld:load=%ld", E.navigations, E.navStartMs, E.commitMs,
        finished, finished - E.navStartMs);
    reportMemory();
    blockingFinished();
    postState();
    schedulePaint();
}

// Stop (or a download taking over, or the page's own redirect) ends a load with an error that is
// not a failure: nothing to show, nothing to report.
bool cancelled(WKErrorRef error)
{
    const int code = error ? WKErrorGetErrorCode(error) : 0;
    return E.stopRequested || code == kWKErrorCodeCancelled || code == kWKErrorCodeFrameLoadInterruptedByPolicyChange;
}

void didFail(const char* what, WKErrorRef error)
{
    E.loading = false;
    if (cancelled(error)) {
        E.stopRequested = false;
        sayHarness("CANCELLED:%s", what);
        postState();
        return;
    }
    std::string description;
    if (error) {
        WKStringRef text = WKErrorCopyLocalizedDescription(error);
        description = utf8(text);
        if (text)
            WKRelease(text);
    }
    // The address that failed: not the page on show (a failed load leaves that in place).
    std::string failing;
    if (error) {
        WKURLRef url = WKErrorCopyFailingURL(error);
        failing = utf8(url);
        if (url)
            WKRelease(url);
    }
    E.error = "Could not load " + (failing.empty() ? std::string("the page") : oneLine(failing)) + ": "
        + (description.empty() ? std::string("unknown error") : oneLine(description));
    say("FAIL:%s:%s:%s (code %d)", what, harness() ? oneLine(failing).c_str() : "", oneLine(description).c_str(),
        error ? WKErrorGetErrorCode(error) : 0);
    postState();
}

void didFailNavigation(WKPageRef, WKNavigationRef, WKErrorRef error, WKTypeRef, const void*) { didFail("navigation", error); }

void didFailProvisionalNavigation(WKPageRef, WKNavigationRef, WKErrorRef error, WKTypeRef, const void*)
{
    // A "save this page" load that failed before a response must not turn the next page into a download.
    E.saveNext = false;
    // The page on show stays, and so does what blocks it (issue #845).
    blockingSettle();
    didFail("provisional", error);
}

// The page's process ended: WebKit says why. The window blanks the page and says so, except when the
// window itself asked (lzw_engine_recover): then the process is replaced by a reload, or the page stays
// stopped.
void webProcessDidTerminate(WKPageRef, WKProcessTerminationReason reason, const void*)
{
    say("WEBPROCESS:GONE:%d:%s", E.webPidShown, terminationReason(reason));
    lzw_launcher_reap();
    dropDialogs();
    E.loading = false;
    E.unresponsive = false;
    lzw_ui_page_responsive(1);
    invalidatePaint();
    if (E.recovering && reason == kWKProcessTerminationReasonRequestedByClient) {
        const bool reload = E.recoverReload;
        E.recovering = false;
        E.recoverReload = false;
        if (reload) {
            E.error.clear();
            E.crashed = false;
            postState();
            // After WebKit has finished with this termination: a reload now would start in the dead process.
            onMain([] { WKPageReload(E.page); });
            return;
        }
        E.crashed = true;
        E.error = "The page was stopped because it was not responding. Reload (F5) starts it again.";
        say("FAIL:webprocess:stopped");
        postState();
        return;
    }
    E.recovering = false;
    E.crashed = true;
    E.error = "The page's process stopped. Reload (F5) starts it again.";
    say("FAIL:webprocess:crashed");
    postState();
}

// WebKit's 6 s ping of the WebProcess went unanswered, or was answered after all.
void responsivenessChanged(const void*)
{
    const bool responsive = WKPageIsWebProcessResponsive(E.page);
    say("WEBPROCESS:%s:%d", responsive ? "RESPONSIVE" : "UNRESPONSIVE", static_cast<int>(WKPageGetProcessIdentifier(E.page)));
    reportResponsiveness();
}

// The NetworkProcess ended. WebKit also kills the one that did not answer for 6 s and starts another
// on demand, so this is the line that says a download or a page load lost its process.
void networkProcessDidCrash(WKContextRef, WKProcessID pid, WKProcessTerminationReason reason, const void*)
{
    say("NETWORKPROCESS:GONE:%d:%s", static_cast<int>(pid), terminationReason(reason));
    lzw_launcher_reap();
}

void stateChanged(const void*) { postState(); }

// The title can change after the load finished (script): say so, once per new title.
void titleChanged(const void*)
{
    if (E.loadFinished && oneLine(pageTitle()) != E.lastTitleSaid)
        sayTitle();
    sayInput();
    postState();
}

} // namespace

void installClients()
{
    static WKPageNavigationClientV3 navigation = { };
    navigation.base.version = 3;
    navigation.decidePolicyForNavigationAction = decidePolicyForNavigationAction;
    navigation.decidePolicyForNavigationResponse = decidePolicyForNavigationResponse;
    navigation.navigationActionDidBecomeDownload = navigationActionDidBecomeDownload;
    navigation.navigationResponseDidBecomeDownload = navigationResponseDidBecomeDownload;
    navigation.didStartProvisionalNavigation = didStartProvisionalNavigation;
    navigation.didCommitNavigation = didCommitNavigation;
    navigation.didFinishNavigation = didFinishNavigation;
    navigation.didFailNavigation = didFailNavigation;
    navigation.didFailProvisionalNavigation = didFailProvisionalNavigation;
    navigation.webProcessDidTerminate = webProcessDidTerminate;
    // What the content rule lists did (issue #845): the count of blocked loads (patch 0018).
    if (has(LZW_FEATURE_BLOCKING))
        navigation.contentRuleListNotification = contentRuleListNotification;
    WKPageSetPageNavigationClient(E.page, &navigation.base);

    static WKPageStateClientV0 state = { };
    state.base.version = 0;
    state.didChangeIsLoading = stateChanged;
    state.didChangeTitle = titleChanged;
    state.didChangeActiveURL = stateChanged;
    state.didChangeEstimatedProgress = stateChanged;
    state.didChangeCanGoBack = stateChanged;
    state.didChangeCanGoForward = stateChanged;
    state.didChangeWebProcessIsResponsive = responsivenessChanged;
    WKPageSetPageStateClient(E.page, &state.base);
}

void installContextClient()
{
    static WKContextClientV4 client = { };
    client.base.version = 4;
    client.networkProcessDidCrashWithDetails = networkProcessDidCrash;
    WKContextSetClient(E.context, &client.base);
}

} // namespace webkitapp
