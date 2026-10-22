// Navigation policy and downloads (docs/lazywebplus.md).
//
// Every navigation asks the window (LZW_FEATURE_POLICY, lzw_ui_navigate) with the kind of
// navigation and whether a user gesture started it: a script must never be able to start another
// program or run one of the browser's commands. A response the page cannot show, or one the
// server sent as an attachment, becomes a download (LZW_FEATURE_DOWNLOADS): the window chooses the
// file in the user's Downloads folder (a ".part" name), the NetworkProcess writes it there, and
// the window renames it when every byte arrived.
//
// An owner may also hold a navigation for a question of its own (webd's `ask`, ViewSink::kVerdictAsk): the
// listener is kept, retained, in the view until the owner answers (View::answerNavigation), and a newer
// main-frame navigation or the view's end refuses it. LazyWeb+'s window never holds one.
//
// Markers (the window prefix first; the ones that name a page only in a UI-probe image):
//   CLICK:<url>        a link was clicked: the navigation it started (a window that prints no markers of its own)
//   DOWNLOAD:REFUSED   the window refused a download

#include <WebKit/WKDownloadClient.h>
#include <WebKit/WKDownloadRef.h>
#include <WebKit/WKErrorRef.h>
#include <WebKit/WKFrameInfoRef.h>
#include <WebKit/WKFramePolicyListener.h>
#include <WebKit/WKNavigationActionRef.h>
#include <WebKit/WKNavigationResponseRef.h>
#include <WebKit/WKPage.h>
#include <WebKit/WKString.h>
#include <WebKit/WKType.h>
#include <WebKit/WKURL.h>
#include <WebKit/WKURLRequest.h>
#include <WebKit/WKURLResponse.h>

#include <stdint.h>
#include <string>

#include "engine.h"
#include "lazyos_ui.h"

namespace webkitapp {

namespace {

// One download's bookkeeping, the client info of its callbacks; freed when it ends.
struct DownloadInfo {
    int view { 0 };
    uint64_t id { 0 };
    bool refused { false };
    // What arrived so far and the size the response announced (-1: none). WebKit's curl download reports each
    // write's bytes and 0 for both totals (NetworkDataTaskCurl.cpp, `didReceiveData(bytesWritten, 0, 0)`), so
    // the totals are counted here.
    int64_t received { 0 };
    int64_t announced { -1 };
};

// The size a response announces, or -1: WebKit's C API answers a 32-bit length, and 0 or all ones
// mean "not announced".
int64_t announcedLength(WKURLResponseRef response)
{
    const uint32_t length = response ? WKURLResponseGetExpectedContentLength(response) : 0;
    return length == 0 || length == 0xffffffffu ? -1 : static_cast<int64_t>(length);
}

DownloadInfo* infoOf(const void* clientInfo) { return static_cast<DownloadInfo*>(const_cast<void*>(clientInfo)); }

// ---- the download client ------------------------------------------------------------------

WKStringRef decideDestination(WKDownloadRef, WKURLResponseRef response, WKStringRef suggestedFilename, const void* clientInfo)
{
    DownloadInfo* info = infoOf(clientInfo);
    WKURLRef url = response ? WKURLResponseCopyURL(response) : nullptr;
    const std::string address = utf8(url);
    if (url)
        WKRelease(url);
    char path[4096] = { 0 };
    View* v = viewById(info->view);
    info->announced = announcedLength(response);
    if (!v || v->sink->downloadStart(info->id, address.c_str(), utf8(suggestedFilename).c_str(), info->announced, path, sizeof path) != 0) {
        info->refused = true;
        say("DOWNLOAD:REFUSED");
        return nullptr;
    }
    return WKStringCreateWithUTF8CString(path);
}

void didWriteData(WKDownloadRef, long long bytesWritten, long long totalBytesWritten, long long totalBytesExpectedToWrite, const void* clientInfo)
{
    DownloadInfo* info = infoOf(clientInfo);
    if (bytesWritten > 0)
        info->received += bytesWritten;
    const int64_t received = totalBytesWritten > 0 ? totalBytesWritten : info->received;
    const int64_t total = totalBytesExpectedToWrite > 0 ? totalBytesExpectedToWrite : info->announced;
    View* v = viewById(info->view);
    if (v && !info->refused)
        v->sink->downloadProgress(info->id, received, total);
}

// The downloads under way, by view and id: what a cancel's completion (below) finds its bookkeeping by.
std::map<std::pair<int, uint64_t>, DownloadInfo*> live;

// The download ended: its view lets go of it (a view that is gone has let go already).
void forget(DownloadInfo* info)
{
    live.erase({ info->view, info->id });
    if (View* v = viewById(info->view)) {
        auto it = v->downloads.find(info->id);
        if (it != v->downloads.end()) {
            WKRelease(it->second);
            v->downloads.erase(it);
        }
    }
    delete info;
}

void didFinish(WKDownloadRef, const void* clientInfo)
{
    DownloadInfo* info = infoOf(clientInfo);
    View* v = viewById(info->view);
    if (v && !info->refused)
        v->sink->downloadDone(info->id);
    forget(info);
}

void didFailWithError(WKDownloadRef, WKErrorRef error, WKDataRef, const void* clientInfo)
{
    DownloadInfo* info = infoOf(clientInfo);
    std::string reason = "unknown error";
    if (error) {
        if (WKErrorGetErrorCode(error) == kWKErrorCodeCancelled) {
            reason = "Cancelled";
        } else {
            WKStringRef text = WKErrorCopyLocalizedDescription(error);
            const std::string description = utf8(text);
            if (text)
                WKRelease(text);
            if (!description.empty())
                reason = oneLine(description);
        }
    }
    View* v = viewById(info->view);
    if (v && !info->refused)
        v->sink->downloadFail(info->id, reason.c_str());
    forget(info);
}

// Wires a new download to the owner: its id, its client.
void adopt(View& v, WKDownloadRef download)
{
    // The navigation became a file: the page on show stays, and so does what blocks it.
    blockingSettle(v);
    auto* info = new DownloadInfo();
    info->view = v.id;
    info->id = v.nextDownload++;
    live[{ info->view, info->id }] = info;
    WKRetain(download);
    v.downloads[info->id] = download;
    WKDownloadClientV0 client = { };
    client.base.version = 0;
    client.base.clientInfo = info;
    client.decideDestinationWithResponse = decideDestination;
    client.didWriteData = didWriteData;
    client.didFinish = didFinish;
    client.didFailWithError = didFailWithError;
    // The client is copied: the next download reuses this struct with its own info.
    WKDownloadSetClient(download, &client.base);
}

// A cancel ends the download through this completion alone: WebKit forgets the download once the
// NetworkProcess answers and calls none of its client's callbacks (DownloadProxy::cancel), so the owner is
// told here, and the bookkeeping freed. A download that ended first (finished, failed) is gone from `live`.
void cancelDone(WKDataRef, const void* context)
{
    std::unique_ptr<std::pair<int, uint64_t>> key(static_cast<std::pair<int, uint64_t>*>(const_cast<void*>(context)));
    auto it = live.find(*key);
    if (it == live.end())
        return;
    DownloadInfo* info = it->second;
    View* v = viewById(info->view);
    if (v && !info->refused)
        v->sink->downloadFail(info->id, "Cancelled");
    forget(info);
}

void cancel(int view, uint64_t id, WKDownloadRef download)
{
    // The completion below frees the bookkeeping the client's callbacks point at: they are taken off
    // first, so a late callback (a write or a failure WebKit delivers after the cancel) finds none.
    WKDownloadSetClient(download, nullptr);
    WKDownloadCancel(download, new std::pair<int, uint64_t>(view, id), cancelDone);
}

// An allowed navigation goes ahead: the rule lists apply or not to the page it begins (issue #845), before
// its first request, and it is loaded, or saved when the page asked for a file.
void finishNavigation(View& v, WKFramePolicyListenerRef listener, const std::string& address, bool mainFrame, bool download)
{
    blockingNavigation(v, address, mainFrame);
    if (download)
        WKFramePolicyListenerDownload(listener);
    else
        WKFramePolicyListenerUse(listener);
}

static_assert(ViewSink::kVerdictDeny == LZW_VERDICT_DENY, "the engine's refusal is lazyos_ui.h's");
static_assert(ViewSink::kVerdictAsk != LZW_VERDICT_ALLOW && ViewSink::kVerdictAsk != LZW_VERDICT_DENY
        && ViewSink::kVerdictAsk != LZW_VERDICT_HANDOFF,
    "holding a navigation is a verdict of its own");

} // namespace

// ---- the navigation client's policy --------------------------------------------------------

void navigationActionDidBecomeDownload(WKPageRef, WKNavigationActionRef, WKDownloadRef download, const void* clientInfo)
{
    adopt(*viewOf(clientInfo), download);
}

void navigationResponseDidBecomeDownload(WKPageRef, WKNavigationResponseRef, WKDownloadRef download, const void* clientInfo)
{
    adopt(*viewOf(clientInfo), download);
}

void decidePolicyForNavigationAction(WKPageRef, WKNavigationActionRef action, WKFramePolicyListenerRef listener, WKTypeRef, const void* clientInfo)
{
    View& v = *viewOf(clientInfo);
    // A light view loads what its owner gave it and nothing else (light.cpp).
    if (v.config.light) {
        lightNavigationAction(v, action, listener);
        return;
    }
    const WKFrameNavigationType type = WKNavigationActionGetNavigationType(action);
    WKURLRequestRef request = WKNavigationActionCopyRequest(action);
    WKURLRef url = request ? WKURLRequestCopyURL(request) : nullptr;
    const std::string address = utf8(url);
    if (type == kWKFrameNavigationTypeLinkClicked)
        sayHarness("CLICK:%s", oneLine(address).c_str());
    else if (type == kWKFrameNavigationTypeFormSubmitted)
        v.formNavigation = true;
    if (url)
        WKRelease(url);
    if (request)
        WKRelease(request);

    const bool gesture = WKNavigationActionHasUnconsumedUserGesture(action);
    // A request for a new window (target=_blank, window.open) has no target frame: createNewPage
    // follows, and needs to know whether a gesture started it.
    WKFrameInfoRef target = WKNavigationActionCopyTargetFrameInfo(action);
    // Only a navigation of the main frame decides about the rule lists. A request for a new window
    // (no target frame) does not replace the page on show: a script's window.open("about:blank")
    // must not switch the blocker off for the page that made it.
    bool mainFrame = false;
    const bool newWindow = !target;
    if (newWindow) {
        v.popupGesture = gesture;
        v.popupUrl = address;
    } else {
        mainFrame = WKFrameInfoGetIsMainFrame(target);
        WKRelease(target);
    }
    // <a download>: the page asked for a file, not a page.
    const bool download = v.has(LZW_FEATURE_DOWNLOADS) && WKNavigationActionShouldPerformDownload(action);

    if (v.has(LZW_FEATURE_POLICY)) {
        // A navigation that replaces the page supersedes the ones still waiting for the owner's answer.
        if (mainFrame)
            dropHeldNavigations(v);
        const std::string committed = committedUrl(v);
        const uint64_t request = v.nextRequest++;
        const int verdict = v.sink->navigateRequest(request, address.c_str(), type, gesture, mainFrame || newWindow, committed.c_str());
        if (verdict == ViewSink::kVerdictAsk) {
            // Neither loaded nor refused: the page's process waits for this one as it waits for any policy.
            WKRetain(listener);
            v.held[request] = HeldNavigation { listener, address, mainFrame, download };
            return;
        }
        if (verdict != LZW_VERDICT_ALLOW) {
            WKFramePolicyListenerIgnore(listener);
            return;
        }
    }
    finishNavigation(v, listener, address, mainFrame, download);
}

void decidePolicyForNavigationResponse(WKPageRef, WKNavigationResponseRef response, WKFramePolicyListenerRef listener, WKTypeRef, const void* clientInfo)
{
    View& v = *viewOf(clientInfo);
    if (v.config.light) {
        lightNavigationResponse(v, response, listener);
        return;
    }
    bool download = false;
    if (v.has(LZW_FEATURE_DOWNLOADS)) {
        WKFrameInfoRef frame = WKNavigationResponseCopyFrameInfo(response);
        const bool mainFrame = !frame || WKFrameInfoGetIsMainFrame(frame);
        if (frame)
            WKRelease(frame);
        if (mainFrame) {
            WKURLResponseRef reply = WKNavigationResponseCopyResponse(response);
            const bool attachment = reply && WKURLResponseIsAttachment(reply);
            if (reply)
                WKRelease(reply);
            download = v.saveNext || attachment || !WKNavigationResponseCanShowMIMEType(response);
            v.saveNext = false;
        }
    }
    if (download)
        WKFramePolicyListenerDownload(listener);
    else
        WKFramePolicyListenerUse(listener);
}

// The page's downloads end with the page: each is cancelled and let go of (its callbacks find the view
// gone and only free their bookkeeping).
void dropDownloads(View& view)
{
    auto pending = std::move(view.downloads);
    view.downloads.clear();
    for (auto& entry : pending) {
        // The cancel's completion finds the view gone: it tells nobody and frees the bookkeeping.
        cancel(view.id, entry.first, entry.second);
        WKRelease(entry.second);
    }
}

void View::download(const std::string& address)
{
    // The next main-frame response is saved, not shown (the response policy above): the page on
    // show stays, as it does when a link to a file is clicked.
    saveNext = true;
    WKURLRef target = WKURLCreateWithUTF8CString(address.c_str());
    WKPageLoadURL(page, target);
    WKRelease(target);
}

void View::downloadCancel(uint64_t id)
{
    auto it = downloads.find(id);
    if (it != downloads.end())
        cancel(this->id, id, it->second);
}

void View::answerNavigation(uint64_t id, bool allow)
{
    auto it = held.find(id);
    if (it == held.end())
        return; // answered, dropped by a newer navigation, or never held
    const HeldNavigation navigation = std::move(it->second);
    held.erase(it);
    if (allow)
        finishNavigation(*this, navigation.listener, navigation.address, navigation.mainFrame, navigation.download);
    else
        WKFramePolicyListenerIgnore(navigation.listener);
    WKRelease(navigation.listener);
}

void dropHeldNavigations(View& v)
{
    auto pending = std::move(v.held);
    v.held.clear();
    for (auto& entry : pending) {
        WKFramePolicyListenerIgnore(entry.second.listener);
        WKRelease(entry.second.listener);
    }
}

} // namespace webkitapp
