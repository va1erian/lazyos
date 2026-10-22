// What the page tells the UI process: navigation and state clients, the markers a session
// reads, and the state handed to the window (docs/webkit-port-plan.md; docs/lazywebplus.md).
//
// Markers (one line each, on stdout, after the window's prefix: WEBPLUS;
// tools/webplus/judge.py reads them):
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

std::string activeUrl(View& v)
{
    WKURLRef url = WKPageCopyActiveURL(v.page);
    std::string text = utf8(url);
    if (url)
        WKRelease(url);
    return text;
}

// Tells the window whether the page's process answers WebKit's pings, as WebKit says now (also when a
// process the window has not heard of appears, and when a page commits: a stale "not responding" must
// not make Reload or Stop end a healthy process).
static void reportResponsiveness(View& v)
{
    const bool responsive = WKPageIsWebProcessResponsive(v.page);
    v.unresponsive = !responsive;
    v.sink->responsive(responsive);
}

// A new page process learns the view's size from its creation parameters, which patch 0030's drawing area
// does not take as a geometry change: it works out how many rows to paint ahead of a scroll (the overscan
// bands, LAZYOS_WEBKIT_OVERSCAN) only in updateGeometry. A view made at its final size and never resized
// (every view of webd: the client gives the size at CreateView) would so get no band at all. One row less
// and back is two geometry updates the process takes as they come; nothing is painted at the odd size
// (paint.cpp paints the view's own size).
static void nudgeGeometry(View& v)
{
    if (!v.view || v.width <= 0 || v.height < 2)
        return;
    WKViewSetSize(v.view, WKSizeMake(v.width, v.height - 1));
    WKViewSetSize(v.view, WKSizeMake(v.width, v.height));
}

void rememberChildren(View& v)
{
    if (!v.page)
        return;
    // A process is announced when WebKit names an id the launcher has started and not seen end, and it
    // is another launch than the one announced (the kernel gives the lowest free slot, so a replacement
    // often takes the dead process's id: the id alone cannot tell). An id the launcher does not hold is a
    // stale answer (WebKit names a process after it ended): not announced, not remembered.
    const int web = static_cast<int>(WKPageGetProcessIdentifier(v.page));
    const unsigned webOrdinal = web > 0 ? processOrdinal("web", web) : 0;
    if (webOrdinal && (web != v.webPidShown || webOrdinal != v.webOrdinalShown)) {
        H.webPid = web;
        v.webPidShown = web;
        v.webOrdinalShown = webOrdinal;
        say("WEBPROCESS:UP:%d", web);
        // A process the owner has not heard of: whatever it was told of the last one (not answering)
        // does not hold for this one.
        reportResponsiveness(v);
        nudgeGeometry(v);
    }
    WKWebsiteDataStoreRef store = v.profile && v.profile->store ? v.profile->store : WKContextGetWebsiteDataStore(H.context);
    const int network = store ? static_cast<int>(WKWebsiteDataStoreGetNetworkProcessIdentifier(store)) : 0;
    const unsigned networkOrdinal = network > 0 ? processOrdinal("network", network) : 0;
    if (networkOrdinal && (network != v.networkPidShown || networkOrdinal != v.networkOrdinalShown)) {
        H.networkPid = network;
        v.networkPidShown = network;
        v.networkOrdinalShown = networkOrdinal;
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

std::string committedUrl(View& v)
{
    WKURLRef url = WKPageCopyCommittedURL(v.page);
    std::string text = utf8(url);
    if (url)
        WKRelease(url);
    return text;
}

std::string pageTitle(View& v)
{
    WKStringRef title = WKPageCopyTitle(v.page);
    std::string text = utf8(title);
    if (title)
        WKRelease(title);
    return text;
}

// ---- state for the window -------------------------------------------------------------

void postState(View& v)
{
    // The WebProcess exists as soon as the page does: when did it come up? A process that replaced
    // another (a crash, a recovery) or a new NetworkProcess is noticed here as well.
    if (!v.webProcessSeen && WKPageGetProcessIdentifier(v.page) > 0) {
        v.webProcessSeen = true;
        say("INFO:TIME:webprocess=%ld", elapsedMs());
    }
    rememberChildren(v);
    std::string url = activeUrl(v);
    std::string committed = committedUrl(v);
    std::string title = pageTitle(v);
    int progress = static_cast<int>(WKPageGetEstimatedProgress(v.page) * 1000.0);
    // A committed https: page was fetched over a verified connection: a certificate that does not
    // verify fails the load (the TLS plan's rule), it never shows a page.
    const bool secure = committed.size() > 8 && committed.compare(0, 8, "https://") == 0;
    ViewState state;
    state.url = url.c_str();
    state.title = title.c_str();
    state.loading = v.loading;
    state.canBack = WKPageCanGoBack(v.page);
    state.canForward = WKPageCanGoForward(v.page);
    state.progress = v.loading ? progress : 1000;
    state.error = v.error.c_str();
    state.crashed = v.crashed;
    state.committedUrl = committed.c_str();
    state.secure = secure;
    state.finished = v.finished;
    state.terminated = v.terminated.c_str();
    state.serial = v.stateSerial;
    v.sink->state(state);
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
    if (!H.env->userMemory(&used, &limit))
        say("INFO:QUOTA:user_memory_mib=%lu:limit_mib=%lu", static_cast<unsigned long>(used >> 20), static_cast<unsigned long>(limit >> 20));
}

void sayTitle(View& v)
{
    std::string title = oneLine(pageTitle(v));
    if (title.empty())
        return;
    sayHarness("TITLE:%s", title.c_str());
    v.lastTitleSaid = title;
}

// The title of the page a form submission led to: what the server echoed back.
void sayInput(View& v)
{
    if (!v.inputPending || !v.loadFinished)
        return;
    std::string title = oneLine(pageTitle(v));
    if (title.empty())
        return;
    v.inputPending = false;
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

void readTitleBySript(View& v)
{
    // Only for a session: it is the proof that script ran in the page, and costs a round trip
    // to the WebProcess per page.
    if (!harness())
        return;
    WKStringRef script = WKStringCreateWithUTF8CString("document.title");
    WKPageEvaluateJavaScriptInMainFrame(v.page, script, nullptr, scriptResult);
    WKRelease(script);
}

void didStartProvisionalNavigation(WKPageRef, WKNavigationRef, WKTypeRef, const void* clientInfo)
{
    View& v = *viewOf(clientInfo);
    dropDialogs(v);
    v.error.clear();
    v.crashed = false;
    v.stopRequested = false;
    v.loading = true;
    v.loadFinished = false;
    sayHarness("START:%s", oneLine(activeUrl(v)).c_str());
    v.navStartMs = elapsedMs();
    v.committedSincePaint = false;
    v.firstPaintMs = -1;
    ++v.navigations;
    postState(v);
}

void didCommitNavigation(WKPageRef, WKNavigationRef, WKTypeRef, const void* clientInfo)
{
    View& v = *viewOf(clientInfo);
    v.saveNext = false; // a page was committed: whatever was to be saved is not this
    v.commitMs = elapsedMs();
    v.committedSincePaint = true;
    reportResponsiveness(v);
    // After redirects the committed address is the one that counts, and it is known before the page's
    // first subresource is asked for.
    blockingSettle(v);
    blockingCommitted(v);
    postState(v);
}

void didFinishNavigation(WKPageRef, WKNavigationRef, WKTypeRef, const void* clientInfo)
{
    View& v = *viewOf(clientInfo);
    v.error.clear();
    v.loading = false;
    v.loadFinished = true;
    ++v.finished;
    long finished = elapsedMs();
    sayHarness("LOAD:%s", oneLine(activeUrl(v)).c_str());
    sayTitle(v);
    if (v.formNavigation) {
        // The echo is the response's title; it can arrive a moment after the load finished.
        v.formNavigation = false;
        v.inputPending = true;
        sayInput(v);
    }
    readTitleBySript(v);
    rememberChildren(v);
    say("INFO:TIME:nav=%d:start=%ld:commit=%ld:finish=%ld:load=%ld", v.navigations, v.navStartMs, v.commitMs,
        finished, finished - v.navStartMs);
    reportMemory();
    blockingFinished(v);
    postState(v);
    schedulePaint(v);
}

// Stop (or a download taking over, or the page's own redirect) ends a load with an error that is
// not a failure: nothing to show, nothing to report.
bool cancelled(View& v, WKErrorRef error)
{
    const int code = error ? WKErrorGetErrorCode(error) : 0;
    return v.stopRequested || code == kWKErrorCodeCancelled || code == kWKErrorCodeFrameLoadInterruptedByPolicyChange;
}

void didFail(View& v, const char* what, WKErrorRef error)
{
    v.loading = false;
    if (cancelled(v, error)) {
        v.stopRequested = false;
        sayHarness("CANCELLED:%s", what);
        postState(v);
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
    v.error = "Could not load " + (failing.empty() ? std::string("the page") : oneLine(failing)) + ": "
        + (description.empty() ? std::string("unknown error") : oneLine(description));
    say("FAIL:%s:%s:%s (code %d)", what, harness() ? oneLine(failing).c_str() : "", oneLine(description).c_str(),
        error ? WKErrorGetErrorCode(error) : 0);
    postState(v);
}

void didFailNavigation(WKPageRef, WKNavigationRef, WKErrorRef error, WKTypeRef, const void* clientInfo)
{
    didFail(*viewOf(clientInfo), "navigation", error);
}

void didFailProvisionalNavigation(WKPageRef, WKNavigationRef, WKErrorRef error, WKTypeRef, const void* clientInfo)
{
    View& v = *viewOf(clientInfo);
    // A "save this page" load that failed before a response must not turn the next page into a download.
    v.saveNext = false;
    // The page on show stays, and so does what blocks it (issue #845).
    blockingSettle(v);
    didFail(v, "provisional", error);
}

// The page's process ended: WebKit says why. The owner blanks the page and says so, except when it
// asked itself (View::recover): then the process is replaced by a reload, or the page stays stopped.
void webProcessDidTerminate(WKPageRef, WKProcessTerminationReason reason, const void* clientInfo)
{
    View& v = *viewOf(clientInfo);
    say("WEBPROCESS:GONE:%d:%s", v.webPidShown, terminationReason(reason));
    v.terminated = terminationReason(reason);
    lzw_launcher_reap();
    // A crashed process the kernel has not finished with yet is still the launcher's: keep collecting.
    processCrashed("web", v.webPidShown);
    dropDialogs(v);
    v.loading = false;
    v.unresponsive = false;
    v.sink->responsive(true);
    invalidatePaint(v);
    if (v.recovering && reason == kWKProcessTerminationReasonRequestedByClient) {
        const bool reload = v.recoverReload;
        v.recovering = false;
        v.recoverReload = false;
        if (reload) {
            v.error.clear();
            v.crashed = false;
            postState(v);
            // After WebKit has finished with this termination: a reload now would start in the dead process.
            onView(v.id, [](View& again) { WKPageReload(again.page); });
            return;
        }
        v.crashed = true;
        v.error = "The page was stopped because it was not responding. Reload (F5) starts it again.";
        say("FAIL:webprocess:stopped");
        postState(v);
        return;
    }
    v.recovering = false;
    v.crashed = true;
    v.error = "The page's process stopped. Reload (F5) starts it again.";
    say("FAIL:webprocess:crashed");
    postState(v);
}

// WebKit's 6 s ping of the WebProcess went unanswered, or was answered after all.
void responsivenessChanged(const void* clientInfo)
{
    View& v = *viewOf(clientInfo);
    const bool responsive = WKPageIsWebProcessResponsive(v.page);
    say("WEBPROCESS:%s:%d", responsive ? "RESPONSIVE" : "UNRESPONSIVE", static_cast<int>(WKPageGetProcessIdentifier(v.page)));
    reportResponsiveness(v);
}

// The NetworkProcess ended. WebKit also kills the one that did not answer for 6 s and starts another
// on demand, so this is the line that says a download or a page load lost its process.
void networkProcessDidCrash(WKContextRef, WKProcessID pid, WKProcessTerminationReason reason, const void*)
{
    say("NETWORKPROCESS:GONE:%d:%s", static_cast<int>(pid), terminationReason(reason));
    lzw_launcher_reap();
    processCrashed("network", static_cast<int>(pid));
}

void stateChanged(const void* clientInfo) { postState(*viewOf(clientInfo)); }

// The title can change after the load finished (script): say so, once per new title.
void titleChanged(const void* clientInfo)
{
    View& v = *viewOf(clientInfo);
    if (v.loadFinished && oneLine(pageTitle(v)) != v.lastTitleSaid)
        sayTitle(v);
    sayInput(v);
    postState(v);
}

} // namespace

void installClients(View& view)
{
    WKPageNavigationClientV3 navigation = { };
    navigation.base.version = 3;
    navigation.base.clientInfo = &view;
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
    if (view.has(LZW_FEATURE_BLOCKING))
        navigation.contentRuleListNotification = contentRuleListNotification;
    WKPageSetPageNavigationClient(view.page, &navigation.base);

    WKPageStateClientV0 state = { };
    state.base.version = 0;
    state.base.clientInfo = &view;
    state.didChangeIsLoading = stateChanged;
    state.didChangeTitle = titleChanged;
    state.didChangeActiveURL = stateChanged;
    state.didChangeEstimatedProgress = stateChanged;
    state.didChangeCanGoBack = stateChanged;
    state.didChangeCanGoForward = stateChanged;
    state.didChangeWebProcessIsResponsive = responsivenessChanged;
    WKPageSetPageStateClient(view.page, &state.base);
}

void installContextClient()
{
    WKContextClientV4 client = { };
    client.base.version = 4;
    client.networkProcessDidCrashWithDetails = networkProcessDidCrash;
    WKContextSetClient(H.context, &client.base);
}

} // namespace webkitapp
