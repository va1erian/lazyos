// The page's UI client and the view's pointer (docs/lazywebplus.md): the link under the pointer, the
// pointer shape, a request for a new window, and the page's JavaScript dialogs.
//
// Dialogs never block this thread: the question goes to the window (lzw_ui_dialog) and the result
// listener is kept, retained, until the window answers (lzw_engine_dialog_reply) or the page it
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
uint64_t keep(int kind, WKTypeRef listener)
{
    WKRetain(listener);
    const uint64_t id = E.nextDialog++;
    E.dialogs[id] = PendingDialog { kind, listener };
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

void runJavaScriptAlert(WKPageRef, WKStringRef text, WKFrameRef, WKSecurityOriginRef, WKPageRunJavaScriptAlertResultListenerRef listener, const void*)
{
    const uint64_t id = keep(LZW_DIALOG_ALERT, listener);
    lzw_ui_dialog(id, LZW_DIALOG_ALERT, utf8(text).c_str(), "");
}

void runJavaScriptConfirm(WKPageRef, WKStringRef text, WKFrameRef, WKSecurityOriginRef, WKPageRunJavaScriptConfirmResultListenerRef listener, const void*)
{
    const uint64_t id = keep(LZW_DIALOG_CONFIRM, listener);
    lzw_ui_dialog(id, LZW_DIALOG_CONFIRM, utf8(text).c_str(), "");
}

void runJavaScriptPrompt(WKPageRef, WKStringRef text, WKStringRef defaultValue, WKFrameRef, WKSecurityOriginRef, WKPageRunJavaScriptPromptResultListenerRef listener, const void*)
{
    const uint64_t id = keep(LZW_DIALOG_PROMPT, listener);
    lzw_ui_dialog(id, LZW_DIALOG_PROMPT, utf8(text).c_str(), utf8(defaultValue).c_str());
}

void mouseDidMoveOverElement(WKPageRef, WKHitTestResultRef hit, WKEventModifiers, WKTypeRef, const void*)
{
    WKURLRef link = hit ? WKHitTestResultCopyAbsoluteLinkURL(hit) : nullptr;
    const std::string address = utf8(link);
    if (link)
        WKRelease(link);
    if (address == E.hover)
        return;
    E.hover = address;
    lzw_ui_hover(address.c_str());
}

// A page asked for a new window. None is made: the window decides what to do with the address
// (LazyWebPlus opens it in its one view when a user gesture started the request).
WKPageRef createNewPage(WKPageRef, WKPageConfigurationRef, WKNavigationActionRef action, WKWindowFeaturesRef, const void*)
{
    WKURLRequestRef request = WKNavigationActionCopyRequest(action);
    WKURLRef url = request ? WKURLRequestCopyURL(request) : nullptr;
    const std::string address = utf8(url);
    if (url)
        WKRelease(url);
    if (request)
        WKRelease(request);
    // The gesture is read where the policy decided on this action; the action itself may have
    // used it up by now.
    const bool gesture = (E.popupUrl == address || address.empty()) ? E.popupGesture : WKNavigationActionHasUnconsumedUserGesture(action);
    E.popupUrl.clear();
    E.popupGesture = false;
    lzw_ui_new_window(address.c_str(), gesture ? 1 : 0);
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

void viewSetCursor(WKViewRef, uint32_t cursorType, const void*) { lzw_ui_cursor(cursorShape(cursorType)); }

void dropDialogs()
{
    // The page that asked is going away (or its process is): its question gets the "no" a closed
    // dialog gives, so nothing waits on a listener that is released.
    auto pending = std::move(E.dialogs);
    E.dialogs.clear();
    for (auto& entry : pending)
        answer(entry.second, false, std::string());
}

void installUiClient()
{
    static WKPageUIClientV6 ui = { };
    ui.base.version = 6;
    if (has(LZW_FEATURE_PAGE_UI)) {
        ui.mouseDidMoveOverElement = mouseDidMoveOverElement;
        ui.createNewPage = createNewPage;
    }
    if (has(LZW_FEATURE_DIALOGS)) {
        ui.runJavaScriptAlert = runJavaScriptAlert;
        ui.runJavaScriptConfirm = runJavaScriptConfirm;
        ui.runJavaScriptPrompt = runJavaScriptPrompt;
    }
    WKPageSetPageUIClient(E.page, &ui.base);
}

} // namespace webkitapp

using namespace webkitapp;

extern "C" void lzw_engine_dialog_reply(uint64_t id, int accepted, const char* value)
{
    std::string text = value ? value : "";
    onMain([id, accepted, text] {
        auto it = E.dialogs.find(id);
        if (it == E.dialogs.end())
            return; // the page went away; its question was answered already
        PendingDialog dialog = it->second;
        E.dialogs.erase(it);
        answer(dialog, accepted != 0, text);
    });
}
