// Navigation policy and downloads (docs/lazywebplus.md).
//
// Every navigation asks the window (LZW_FEATURE_POLICY, lzw_ui_navigate) with the kind of
// navigation and whether a user gesture started it: a script must never be able to start another
// program or run one of the browser's commands. A response the page cannot show, or one the
// server sent as an attachment, becomes a download (LZW_FEATURE_DOWNLOADS): the window chooses the
// file in the user's Downloads folder (a ".part" name), the NetworkProcess writes it there, and
// the window renames it when every byte arrived.
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
    if (!v || v->sink->downloadStart(info->id, address.c_str(), utf8(suggestedFilename).c_str(), announcedLength(response), path, sizeof path) != 0) {
        info->refused = true;
        say("DOWNLOAD:REFUSED");
        return nullptr;
    }
    return WKStringCreateWithUTF8CString(path);
}

void didWriteData(WKDownloadRef, long long, long long totalBytesWritten, long long totalBytesExpectedToWrite, const void* clientInfo)
{
    DownloadInfo* info = infoOf(clientInfo);
    View* v = viewById(info->view);
    if (v && !info->refused)
        v->sink->downloadProgress(info->id, totalBytesWritten, totalBytesExpectedToWrite > 0 ? totalBytesExpectedToWrite : -1);
}

// The download ended: its view lets go of it (a view that is gone has let go already).
void forget(DownloadInfo* info)
{
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
    if (!target) {
        v.popupGesture = gesture;
        v.popupUrl = address;
    } else {
        mainFrame = WKFrameInfoGetIsMainFrame(target);
        WKRelease(target);
    }

    if (v.has(LZW_FEATURE_POLICY)) {
        const std::string committed = committedUrl(v);
        if (v.sink->navigate(address.c_str(), type, gesture, committed.c_str()) != LZW_VERDICT_ALLOW) {
            WKFramePolicyListenerIgnore(listener);
            return;
        }
    }
    // The rule lists apply or not to the page this begins (issue #845), before its first request.
    blockingNavigation(v, address, mainFrame);
    // <a download>: the page asked for a file, not a page.
    if (v.has(LZW_FEATURE_DOWNLOADS) && WKNavigationActionShouldPerformDownload(action))
        WKFramePolicyListenerDownload(listener);
    else
        WKFramePolicyListenerUse(listener);
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
        WKDownloadCancel(entry.second, nullptr, [](WKDataRef, const void*) { });
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
        WKDownloadCancel(it->second, nullptr, [](WKDataRef, const void*) { });
}

} // namespace webkitapp
