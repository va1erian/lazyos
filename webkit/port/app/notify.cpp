// The Notifications API (docs/lazywebplus.md, "Notifications"): WebKit's notification provider and
// the page's permission requests, forwarded to the window (LZW_FEATURE_NOTIFICATIONS).
//
// WebKit has the whole machinery in the UI process (`WKNotificationManager`, the provider, the UI
// client's `decidePolicyForNotificationPermissionRequest`) but a port must supply the provider, or
// every page sees `Notification.permission === "denied"` and nothing shows. The window decides
// everything (which sites may notify, what a toast says, how many); this file only translates:
//
//   page -> window   lzw_ui_notify_show / _cancel / _gone / _permission_request
//   window -> page   lzw_engine_notify_shown / _clicked / _closed / _permission / _forget
//
// A permission request is held, retained, until the window answers; the answer for an origin
// settles every pending request of that origin at once. The decisions the window kept are given to
// WebKit when it asks (`notificationPermissions`, read when a page's process starts) and told to it
// when they change (`WKNotificationManagerProviderDidUpdateNotificationPolicy`).
//
// Nothing here is persisted and nothing is shown by this file. A page's notification is only ever
// reported with the origin WebKit itself attributes it to, never with a name the page chose.

#include <WebKit/WKArray.h>
#include <WebKit/WKContext.h>
#include <WebKit/WKDictionary.h>
#include <WebKit/WKMutableDictionary.h>
#include <WebKit/WKNotification.h>
#include <WebKit/WKNotificationManager.h>
#include <WebKit/WKNotificationPermissionRequest.h>
#include <WebKit/WKNotificationProvider.h>
#include <WebKit/WKNumber.h>
#include <WebKit/WKPage.h>
#include <WebKit/WKPageUIClient.h>
#include <WebKit/WKSecurityOriginRef.h>
#include <WebKit/WKString.h>
#include <WebKit/WKType.h>
#include <WebKit/WKURL.h>

#include <stdint.h>
#include <string>
#include <utility>
#include <vector>

#include "engine.h"
#include "lazyos_ui.h"

namespace webkitapp {

namespace {

// The most permission requests held at once: a page needs a user gesture for each, so this is a
// ceiling nothing reaches by hand.
constexpr size_t kMaxPendingRequests = 16;

std::string originOf(WKSecurityOriginRef origin)
{
    if (!origin)
        return std::string();
    WKStringRef text = WKSecurityOriginCopyToString(origin);
    std::string result = utf8(text);
    if (text)
        WKRelease(text);
    return result;
}

std::string copied(WKStringRef text)
{
    std::string result = utf8(text);
    if (text)
        WKRelease(text);
    return result;
}

// ---- the provider: what a page asks to show ----------------------------------------------

void providerShow(WKPageRef page, WKNotificationRef notification, const void*)
{
    if (!notification || WKNotificationGetIsPersistent(notification))
        return; // a service worker's: there is no page to bring back, and v1 shows none
    const uint64_t id = WKNotificationGetID(notification);
    const std::string origin = originOf(WKNotificationGetSecurityOrigin(notification));
    std::string pageUrl;
    if (page) {
        WKURLRef url = WKPageCopyCommittedURL(page);
        pageUrl = utf8(url);
        if (url)
            WKRelease(url);
    }
    const std::string title = copied(WKNotificationCopyTitle(notification));
    const std::string body = copied(WKNotificationCopyBody(notification));
    const std::string tag = copied(WKNotificationCopyTag(notification));
    lzw_ui_notify_show(id, origin.c_str(), pageUrl.c_str(), title.c_str(), body.c_str(), tag.c_str());
}

// The page took it down (`Notification.close()`), or the page is being suspended into the
// back/forward cache, which closes its notifications the same way (`Notification::suspend`): a
// call that comes while a navigation is in progress is the second, and the toast, which must
// outlive its page, stays as the notification of a page that is gone.
void providerCancel(WKNotificationRef notification, const void*)
{
    if (!notification)
        return;
    const uint64_t id = WKNotificationGetID(notification);
    if (E.loading)
        lzw_ui_notify_gone(id);
    else
        lzw_ui_notify_cancel(id);
}

// The page's object is gone (its document was): the toast, if any, is the person's now.
void providerDidDestroy(WKNotificationRef notification, const void*)
{
    if (notification)
        lzw_ui_notify_gone(WKNotificationGetID(notification));
}

void providerAddManager(WKNotificationManagerRef manager, const void*) { E.notifications = manager; }

void providerRemoveManager(WKNotificationManagerRef manager, const void*)
{
    if (E.notifications == manager)
        E.notifications = nullptr;
}

// The decisions the window kept, as WebKit wants them: origin string to boolean. The dictionary
// is handed over retained (WebKit adopts it).
WKDictionaryRef providerPermissions(const void*)
{
    WKMutableDictionaryRef dictionary = WKMutableDictionaryCreate();
    char origin[512];
    for (size_t index = 0; index < 4096; ++index) {
        const int decision = lzw_ui_notify_permission_at(index, origin, sizeof origin);
        if (decision == -1)
            break;
        if (decision < 0)
            continue;
        WKStringRef key = WKStringCreateWithUTF8CString(origin);
        WKBooleanRef value = WKBooleanCreate(decision != 0);
        WKDictionarySetItem(dictionary, key, value);
        WKRelease(value);
        WKRelease(key);
    }
    return dictionary;
}

// "Clear" (all of a page's notifications are to go): the window takes a page's toasts down as the
// page's objects go, so there is nothing more to do here.
void providerClear(WKArrayRef, const void*) { }

// ---- the page's permission request ---------------------------------------------------------

} // namespace

void notificationPermissionRequest(WKPageRef, WKSecurityOriginRef origin, WKNotificationPermissionRequestRef request, const void*)
{
    if (!request)
        return;
    if (E.permissionRequests.size() >= kMaxPendingRequests) {
        WKNotificationPermissionRequestDeny(request);
        return;
    }
    const std::string text = originOf(origin);
    WKRetain(request);
    E.permissionRequests.emplace_back(text, request);
    // WebKit has already required a user gesture (Notification.requestPermission is refused
    // without a transient activation); the window looks at the input it forwarded as well.
    lzw_ui_notify_permission_request(text.c_str(), 1);
}

namespace {

// Answers every pending request of `origin`: allowed, or not; whether the decision is remembered is
// the caller's business.
void settle(const std::string& origin, bool allow)
{
    std::vector<std::pair<std::string, WKNotificationPermissionRequestRef>> keep;
    auto pending = std::move(E.permissionRequests);
    E.permissionRequests.clear();
    for (auto& entry : pending) {
        if (entry.first != origin) {
            keep.push_back(entry);
            continue;
        }
        if (allow)
            WKNotificationPermissionRequestAllow(entry.second);
        else
            WKNotificationPermissionRequestDeny(entry.second);
        WKRelease(entry.second);
    }
    E.permissionRequests = std::move(keep);
}

WKSecurityOriginRef originFromString(const std::string& text)
{
    WKStringRef string = WKStringCreateWithUTF8CString(text.c_str());
    WKSecurityOriginRef origin = WKSecurityOriginCreateFromString(string);
    WKRelease(string);
    return origin;
}

} // namespace

void installNotifications()
{
    if (!has(LZW_FEATURE_NOTIFICATIONS))
        return;
    static WKNotificationProviderV0 provider = { };
    provider.base.version = 0;
    provider.show = providerShow;
    provider.cancel = providerCancel;
    provider.didDestroyNotification = providerDidDestroy;
    provider.addNotificationManager = providerAddManager;
    provider.removeNotificationManager = providerRemoveManager;
    provider.notificationPermissions = providerPermissions;
    provider.clearNotifications = providerClear;
    WKNotificationManagerRef manager = WKContextGetNotificationManager(E.context);
    E.notifications = manager;
    WKNotificationManagerSetProvider(manager, &provider.base);
    // A page's process may already be running: it learns the decisions now, not at its next start.
    char origin[512];
    for (size_t index = 0; index < 4096; ++index) {
        const int decision = lzw_ui_notify_permission_at(index, origin, sizeof origin);
        if (decision == -1)
            break;
        if (decision < 0)
            continue;
        WKSecurityOriginRef site = originFromString(origin);
        WKNotificationManagerProviderDidUpdateNotificationPolicy(manager, site, decision != 0);
        WKRelease(site);
    }
}

// The page that asked is going away: its requests get the answer a closed bar gives, and the
// window hears that its question is moot (once per site).
void dropNotificationRequests()
{
    auto pending = std::move(E.permissionRequests);
    E.permissionRequests.clear();
    std::vector<std::string> told;
    for (auto& entry : pending) {
        WKNotificationPermissionRequestDeny(entry.second);
        WKRelease(entry.second);
        bool already = false;
        for (const auto& origin : told)
            already = already || origin == entry.first;
        if (!already) {
            told.push_back(entry.first);
            lzw_ui_notify_permission_gone(entry.first.c_str());
        }
    }
}

} // namespace webkitapp

using namespace webkitapp;

extern "C" {

void lzw_engine_notify_shown(uint64_t id)
{
    onMain([id] {
        if (E.notifications)
            WKNotificationManagerProviderDidShowNotification(E.notifications, id);
    });
}

void lzw_engine_notify_clicked(uint64_t id)
{
    onMain([id] {
        if (E.notifications)
            WKNotificationManagerProviderDidClickNotification(E.notifications, id);
    });
}

void lzw_engine_notify_closed(uint64_t id)
{
    onMain([id] {
        if (!E.notifications)
            return;
        WKUInt64Ref value = WKUInt64Create(id);
        WKTypeRef values[] = { value };
        WKArrayRef array = WKArrayCreate(values, 1);
        WKNotificationManagerProviderDidCloseNotifications(E.notifications, array);
        WKRelease(array);
        WKRelease(value);
    });
}

// answer: 1 allow, 0 block (both remembered), -1 dismiss (the pending requests are refused and
// nothing is remembered: the site may ask again).
void lzw_engine_notify_permission(const char* origin, int answer)
{
    const std::string text = origin ? origin : "";
    onMain([text, answer] {
        // The policy first: the WebProcess handles its messages in order, and the page's
        // continuation (which makes `new Notification` look the permission up) must run after it.
        if (answer >= 0 && E.notifications && !text.empty()) {
            WKSecurityOriginRef site = originFromString(text);
            WKNotificationManagerProviderDidUpdateNotificationPolicy(E.notifications, site, answer > 0);
            WKRelease(site);
        }
        settle(text, answer > 0);
    });
}

// The person revoked the decision about `origin`: the pages of that site get "default" again.
void lzw_engine_notify_forget(const char* origin)
{
    const std::string text = origin ? origin : "";
    onMain([text] {
        if (!E.notifications || text.empty())
            return;
        WKSecurityOriginRef site = originFromString(text);
        WKTypeRef values[] = { site };
        WKArrayRef array = WKArrayCreate(values, 1);
        WKNotificationManagerProviderDidRemoveNotificationPolicies(E.notifications, array);
        WKRelease(array);
        WKRelease(site);
    });
}

} // extern "C"
