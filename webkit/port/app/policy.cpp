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
//   CLICK:<url>        a link was clicked: the navigation it started (W5's window only)
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
    if (lzw_ui_download_start(info->id, address.c_str(), utf8(suggestedFilename).c_str(), announcedLength(response), path,
            sizeof path)
        != 0) {
        info->refused = true;
        say("DOWNLOAD:REFUSED");
        return nullptr;
    }
    return WKStringCreateWithUTF8CString(path);
}

void didWriteData(WKDownloadRef, long long, long long totalBytesWritten, long long totalBytesExpectedToWrite, const void* clientInfo)
{
    DownloadInfo* info = infoOf(clientInfo);
    if (!info->refused)
        lzw_ui_download_progress(info->id, totalBytesWritten, totalBytesExpectedToWrite > 0 ? totalBytesExpectedToWrite : -1);
}

void forget(DownloadInfo* info)
{
    auto it = E.downloads.find(info->id);
    if (it != E.downloads.end()) {
        WKRelease(it->second);
        E.downloads.erase(it);
    }
    delete info;
}

void didFinish(WKDownloadRef, const void* clientInfo)
{
    DownloadInfo* info = infoOf(clientInfo);
    if (!info->refused)
        lzw_ui_download_done(info->id);
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
    if (!info->refused)
        lzw_ui_download_fail(info->id, reason.c_str());
    forget(info);
}

// Wires a new download to the window: its id, its client.
void adopt(WKDownloadRef download)
{
    auto* info = new DownloadInfo();
    info->id = E.nextDownload++;
    WKRetain(download);
    E.downloads[info->id] = download;
    static WKDownloadClientV0 client = { };
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

void navigationActionDidBecomeDownload(WKPageRef, WKNavigationActionRef, WKDownloadRef download, const void*) { adopt(download); }

void navigationResponseDidBecomeDownload(WKPageRef, WKNavigationResponseRef, WKDownloadRef download, const void*) { adopt(download); }

void decidePolicyForNavigationAction(WKPageRef, WKNavigationActionRef action, WKFramePolicyListenerRef listener, WKTypeRef, const void*)
{
    const WKFrameNavigationType type = WKNavigationActionGetNavigationType(action);
    WKURLRequestRef request = WKNavigationActionCopyRequest(action);
    WKURLRef url = request ? WKURLRequestCopyURL(request) : nullptr;
    const std::string address = utf8(url);
    if (type == kWKFrameNavigationTypeLinkClicked)
        sayHarness("CLICK:%s", oneLine(address).c_str());
    else if (type == kWKFrameNavigationTypeFormSubmitted)
        E.formNavigation = true;
    if (url)
        WKRelease(url);
    if (request)
        WKRelease(request);

    const bool gesture = WKNavigationActionHasUnconsumedUserGesture(action);
    // A request for a new window (target=_blank, window.open) has no target frame: createNewPage
    // follows, and needs to know whether a gesture started it.
    WKFrameInfoRef target = WKNavigationActionCopyTargetFrameInfo(action);
    if (!target) {
        E.popupGesture = gesture;
        E.popupUrl = address;
    } else {
        WKRelease(target);
    }

    if (has(LZW_FEATURE_POLICY)) {
        const std::string committed = committedUrl();
        if (lzw_ui_navigate(address.c_str(), type, gesture ? 1 : 0, committed.c_str()) != LZW_VERDICT_ALLOW) {
            WKFramePolicyListenerIgnore(listener);
            return;
        }
    }
    // <a download>: the page asked for a file, not a page.
    if (has(LZW_FEATURE_DOWNLOADS) && WKNavigationActionShouldPerformDownload(action))
        WKFramePolicyListenerDownload(listener);
    else
        WKFramePolicyListenerUse(listener);
}

void decidePolicyForNavigationResponse(WKPageRef, WKNavigationResponseRef response, WKFramePolicyListenerRef listener, WKTypeRef, const void*)
{
    bool download = false;
    if (has(LZW_FEATURE_DOWNLOADS)) {
        WKFrameInfoRef frame = WKNavigationResponseCopyFrameInfo(response);
        const bool mainFrame = !frame || WKFrameInfoGetIsMainFrame(frame);
        if (frame)
            WKRelease(frame);
        if (mainFrame) {
            WKURLResponseRef reply = WKNavigationResponseCopyResponse(response);
            const bool attachment = reply && WKURLResponseIsAttachment(reply);
            if (reply)
                WKRelease(reply);
            download = E.saveNext || attachment || !WKNavigationResponseCanShowMIMEType(response);
            E.saveNext = false;
        }
    }
    if (download)
        WKFramePolicyListenerDownload(listener);
    else
        WKFramePolicyListenerUse(listener);
}

} // namespace webkitapp

using namespace webkitapp;

extern "C" {

void lzw_engine_download(const char* url)
{
    std::string address = url ? url : "";
    onMain([address] {
        // The next main-frame response is saved, not shown (the response policy above): the page on
        // show stays, as it does when a link to a file is clicked.
        E.saveNext = true;
        WKURLRef target = WKURLCreateWithUTF8CString(address.c_str());
        WKPageLoadURL(E.page, target);
        WKRelease(target);
    });
}

void lzw_engine_download_cancel(uint64_t id)
{
    onMain([id] {
        auto it = E.downloads.find(id);
        if (it != E.downloads.end())
            WKDownloadCancel(it->second, nullptr, [](WKDataRef, const void*) { });
    });
}

} // extern "C"
