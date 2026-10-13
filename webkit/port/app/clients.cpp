// What the page tells the UI process: navigation and state clients, the markers a session
// reads, and the state handed to the window (docs/webkit-port-plan.md, W5).
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
//   WEBKIT:W5:INFO:MEM:...              the guest's RAM in MiB (sysinfo): total, free, and what this
//                                       process and its children took since the UI process started

#include <WebKit/WKContext.h>
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
    // The WebProcess exists as soon as the page does: when did it come up?
    static bool webProcessSeen = false;
    if (!webProcessSeen && WKPageGetProcessIdentifier(E.page) > 0) {
        webProcessSeen = true;
        say("WEBKIT:W5:INFO:TIME:webprocess=%ld", elapsedMs());
    }
    std::string url = activeUrl();
    std::string title = pageTitle();
    int progress = static_cast<int>(WKPageGetEstimatedProgress(E.page) * 1000.0);
    lzw_ui_state(url.c_str(), title.c_str(), E.loading ? 1 : 0, WKPageCanGoBack(E.page) ? 1 : 0,
        WKPageCanGoForward(E.page) ? 1 : 0, E.loading ? progress : 1000);
}

namespace {

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
    say("WEBKIT:W5:INFO:MEM:total=%ld:free=%ld:used_since_first_load=%ld", totalMiB, freeMiB, firstFreeMiB - freeMiB);
    uint64_t used = 0, limit = 0;
    if (!lzw_ui_user_memory(&used, &limit))
        say("WEBKIT:W5:INFO:QUOTA:user_memory_mib=%lu:limit_mib=%lu", static_cast<unsigned long>(used >> 20), static_cast<unsigned long>(limit >> 20));
}

void sayTitle()
{
    std::string title = oneLine(pageTitle());
    if (title.empty())
        return;
    say("WEBKIT:W5:TITLE:%s", title.c_str());
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
    say("WEBKIT:W5:INPUT:%s", title.c_str());
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
        // The echo is the response's title; it can arrive a moment after the load finished.
        E.formNavigation = false;
        E.inputPending = true;
        sayInput();
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
    sayInput();
    postState();
}

} // namespace

void installClients()
{
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
}

} // namespace webkitapp
