// The page's UI client and the view's pointer (docs/lazywebplus.md): the link under the pointer, the
// pointer shape, a request for a new window, the page's JavaScript dialogs, and (notify.cpp's
// callback) the permission to notify.
//
// Dialogs never block this thread: the question goes to the owner's sink and the result
// listener is kept, retained, until the owner answers or the page it
// belongs to goes away (dropDialogs). The page's own process waits, which is what a dialog is;
// nothing else does, so the frame mailbox keeps flowing.

#include <WebKit/WKFrame.h>
#include <WebKit/WKHitTestResult.h>
#include <WebKit/WKNavigationActionRef.h>
#include <WebKit/WKPage.h>
#include <WebKit/WKPageUIClient.h>
#include <WebKit/WKSecurityOriginRef.h>
#include <WebKit/WKString.h>
#include <WebKit/WKType.h>
#include <WebKit/WKURL.h>
#include <WebKit/WKURLRequest.h>
#include <WebKit/WKViewClient.h>

#include <stdint.h>
#include <string>

#include "engine.h"
#include "lazyos_ui.h"

namespace webkitapp {

namespace {

// Keeps a dialog's listener for the window's answer; returns its id.
uint64_t keep(View& v, int kind, WKTypeRef listener)
{
    WKRetain(listener);
    const uint64_t id = v.nextDialog++;
    v.dialogs[id] = PendingDialog { kind, listener };
    return id;
}

// Gives a pending dialog its answer (the page's default for "no") and lets it go.
void answer(PendingDialog& dialog, bool accepted, const std::string& value)
{
    switch (dialog.kind) {
    case LZW_DIALOG_ALERT:
        WKPageRunJavaScriptAlertResultListenerCall(static_cast<WKPageRunJavaScriptAlertResultListenerRef>(dialog.listener));
        break;
    case LZW_DIALOG_CONFIRM:
        WKPageRunJavaScriptConfirmResultListenerCall(static_cast<WKPageRunJavaScriptConfirmResultListenerRef>(dialog.listener), accepted);
        break;
    default: {
        // A null string is Cancel; an empty one is OK with nothing typed.
        WKStringRef text = accepted ? WKStringCreateWithUTF8CString(value.c_str()) : nullptr;
        WKPageRunJavaScriptPromptResultListenerCall(static_cast<WKPageRunJavaScriptPromptResultListenerRef>(dialog.listener), text);
        if (text)
            WKRelease(text);
        break;
    }
    }
    WKRelease(dialog.listener);
    dialog.listener = nullptr;
}

void runJavaScriptAlert(WKPageRef, WKStringRef text, WKFrameRef, WKSecurityOriginRef, WKPageRunJavaScriptAlertResultListenerRef listener, const void* clientInfo)
{
    View& v = *viewOf(clientInfo);
    const uint64_t id = keep(v, LZW_DIALOG_ALERT, listener);
    v.sink->dialog(id, LZW_DIALOG_ALERT, utf8(text).c_str(), "");
}

void runJavaScriptConfirm(WKPageRef, WKStringRef text, WKFrameRef, WKSecurityOriginRef, WKPageRunJavaScriptConfirmResultListenerRef listener, const void* clientInfo)
{
    View& v = *viewOf(clientInfo);
    const uint64_t id = keep(v, LZW_DIALOG_CONFIRM, listener);
    v.sink->dialog(id, LZW_DIALOG_CONFIRM, utf8(text).c_str(), "");
}

void runJavaScriptPrompt(WKPageRef, WKStringRef text, WKStringRef defaultValue, WKFrameRef, WKSecurityOriginRef, WKPageRunJavaScriptPromptResultListenerRef listener, const void* clientInfo)
{
    View& v = *viewOf(clientInfo);
    const uint64_t id = keep(v, LZW_DIALOG_PROMPT, listener);
    v.sink->dialog(id, LZW_DIALOG_PROMPT, utf8(text).c_str(), utf8(defaultValue).c_str());
}

void mouseDidMoveOverElement(WKPageRef, WKHitTestResultRef hit, WKEventModifiers, WKTypeRef, const void* clientInfo)
{
    View& v = *viewOf(clientInfo);
    WKURLRef link = hit ? WKHitTestResultCopyAbsoluteLinkURL(hit) : nullptr;
    const std::string address = utf8(link);
    if (link)
        WKRelease(link);
    if (address == v.hover)
        return;
    v.hover = address;
    v.sink->hover(address.c_str());
}

// A page asked for a new window. None is made: the owner decides what to do with the address
// (LazyWebPlus opens it in its one view when a user gesture started the request).
WKPageRef createNewPage(WKPageRef, WKPageConfigurationRef, WKNavigationActionRef action, WKWindowFeaturesRef, const void* clientInfo)
{
    View& v = *viewOf(clientInfo);
    WKURLRequestRef request = WKNavigationActionCopyRequest(action);
    WKURLRef url = request ? WKURLRequestCopyURL(request) : nullptr;
    const std::string address = utf8(url);
    if (url)
        WKRelease(url);
    if (request)
        WKRelease(request);
    // The gesture is read where the policy decided on this action; the action itself may have
    // used it up by now.
    const bool gesture = (v.popupUrl == address || address.empty()) ? v.popupGesture : WKNavigationActionHasUnconsumedUserGesture(action);
    v.popupUrl.clear();
    v.popupGesture = false;
    v.sink->newWindow(address.c_str(), gesture);
    return nullptr;
}

int cursorShape(uint32_t type)
{
    switch (type) {
    case kWKCursorTypeHand:
        return LZW_CURSOR_HAND;
    case kWKCursorTypeIBeam:
        return LZW_CURSOR_TEXT;
    case kWKCursorTypeWait:
    case kWKCursorTypeProgress:
        return LZW_CURSOR_WAIT;
    case kWKCursorTypeNone:
        return LZW_CURSOR_HIDDEN;
    case kWKCursorTypeEastWestResize:
        return LZW_CURSOR_RESIZE_H;
    case kWKCursorTypeNorthSouthResize:
        return LZW_CURSOR_RESIZE_V;
    case kWKCursorTypeNorthWestSouthEastResize:
        return LZW_CURSOR_RESIZE_NWSE;
    case kWKCursorTypeNorthEastSouthWestResize:
        return LZW_CURSOR_RESIZE_NESW;
    default:
        return LZW_CURSOR_DEFAULT;
    }
}

} // namespace

void viewSetCursor(WKViewRef, uint32_t cursorType, const void* clientInfo)
{
    viewOf(clientInfo)->sink->cursor(cursorShape(cursorType));
}

void dropDialogs(View& v)
{
    // A request to notify belongs to the page that asked too.
    dropNotificationRequests(v);
    // The page that asked is going away (or its process is): its question gets the "no" a closed
    // dialog gives, so nothing waits on a listener that is released.
    auto pending = std::move(v.dialogs);
    v.dialogs.clear();
    for (auto& entry : pending)
        answer(entry.second, false, std::string());
}

void installUiClient(View& view)
{
    WKPageUIClientV6 ui = { };
    ui.base.version = 6;
    ui.base.clientInfo = &view;
    if (view.has(LZW_FEATURE_PAGE_UI)) {
        ui.mouseDidMoveOverElement = mouseDidMoveOverElement;
        ui.createNewPage = createNewPage;
    }
    if (view.has(LZW_FEATURE_DIALOGS)) {
        ui.runJavaScriptAlert = runJavaScriptAlert;
        ui.runJavaScriptConfirm = runJavaScriptConfirm;
        ui.runJavaScriptPrompt = runJavaScriptPrompt;
    }
    if (view.has(LZW_FEATURE_NOTIFICATIONS))
        ui.decidePolicyForNotificationPermissionRequest = notificationPermissionRequest;
    WKPageSetPageUIClient(view.page, &ui.base);
}

void View::replyDialog(uint64_t id, bool accepted, const std::string& value)
{
    auto it = dialogs.find(id);
    if (it == dialogs.end())
        return; // the page went away; its question was answered already
    PendingDialog dialog = it->second;
    dialogs.erase(it);
    answer(dialog, accepted, value);
}

} // namespace webkitapp
