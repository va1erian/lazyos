// The Notifications API (docs/lazywebplus.md, "Notifications"): WebKit's notification provider and
// the page's permission requests, forwarded to the window (LZW_FEATURE_NOTIFICATIONS).
//
// WebKit has the whole machinery in the UI process (`WKNotificationManager`, the provider, the UI
// client's `decidePolicyForNotificationPermissionRequest`) but a port must supply the provider, or
// every page sees `Notification.permission === "denied"` and nothing shows. The owner decides
// everything (which sites may notify, what a toast says, how many); this file only translates:
//
//   page -> owner   ViewSink::notifyShow / notifyCancel / notifyGone / notifyPermissionRequest
//   owner -> page   notifyShown / notifyClicked / notifyClosed / notifyPermission / notifyForget
//
// The manager is the context's, one for every view; a notification is the view's whose page showed it
// (H.notificationViews). A permission request is held, retained, by its view until the owner answers;
// the answer for an origin settles every pending request of that origin, in every view, at once. The decisions the window kept are given to
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

// The view a live notification belongs to, or null (its view or the notification is gone).
View* ownerOf(uint64_t id)
{
    auto it = H.notificationViews.find(id);
    return it == H.notificationViews.end() ? nullptr : viewById(it->second);
}

void providerShow(WKPageRef page, WKNotificationRef notification, const void*)
{
    if (!notification || WKNotificationGetIsPersistent(notification))
        return; // a service worker's: there is no page to bring back, and v1 shows none
    View* view = page ? viewOfPage(page) : nullptr;
    if (!view || !view->has(LZW_FEATURE_NOTIFICATIONS))
        return;
    const uint64_t id = WKNotificationGetID(notification);
    H.notificationViews[id] = view->id;
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
    view->sink->notifyShow(id, origin.c_str(), pageUrl.c_str(), title.c_str(), body.c_str(), tag.c_str());
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
    View* view = ownerOf(id);
    if (!view)
        return;
    if (view->loading)
        view->sink->notifyGone(id);
    else
        view->sink->notifyCancel(id);
}

// The page's object is gone (its document was): the toast, if any, is the person's now.
void providerDidDestroy(WKNotificationRef notification, const void*)
{
    if (!notification)
        return;
    const uint64_t id = WKNotificationGetID(notification);
    if (View* view = ownerOf(id))
        view->sink->notifyGone(id);
    H.notificationViews.erase(id);
}

void providerAddManager(WKNotificationManagerRef manager, const void*) { H.notifications = manager; }

void providerRemoveManager(WKNotificationManagerRef manager, const void*)
{
    if (H.notifications == manager)
        H.notifications = nullptr;
}

// The decisions the window kept, as WebKit wants them: origin string to boolean. The dictionary
// is handed over retained (WebKit adopts it).
WKDictionaryRef providerPermissions(const void*)
{
    WKMutableDictionaryRef dictionary = WKMutableDictionaryCreate();
    char origin[512];
    for (size_t index = 0; index < 4096; ++index) {
        const int decision = H.env->notifyPermissionAt(index, origin, sizeof origin);
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

void notificationPermissionRequest(WKPageRef, WKSecurityOriginRef origin, WKNotificationPermissionRequestRef request, const void* clientInfo)
{
    if (!request)
        return;
    View& v = *viewOf(clientInfo);
    if (v.permissionRequests.size() >= kMaxPendingRequests) {
        WKNotificationPermissionRequestDeny(request);
        return;
    }
    const std::string text = originOf(origin);
    WKRetain(request);
    v.permissionRequests.emplace_back(text, request);
    // WebKit has already required a user gesture (Notification.requestPermission is refused
    // without a transient activation); the owner looks at the input it forwarded as well.
    v.sink->notifyPermissionRequest(text.c_str());
}

namespace {

// Answers every pending request of `origin`, in every view: allowed, or not; whether the decision is
// remembered is the caller's business.
void settle(const std::string& origin, bool allow)
{
    for (auto& viewEntry : H.views) {
        View& v = *viewEntry.second;
        std::vector<std::pair<std::string, WKNotificationPermissionRequestRef>> keep;
        auto pending = std::move(v.permissionRequests);
        v.permissionRequests.clear();
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
        v.permissionRequests = std::move(keep);
    }
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
    WKNotificationManagerRef manager = WKContextGetNotificationManager(H.context);
    H.notifications = manager;
    WKNotificationManagerSetProvider(manager, &provider.base);
    // A page's process may already be running: it learns the decisions now, not at its next start.
    char origin[512];
    for (size_t index = 0; index < 4096; ++index) {
        const int decision = H.env->notifyPermissionAt(index, origin, sizeof origin);
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
// owner hears that its question is moot (once per site).
void dropNotificationRequests(View& v)
{
    auto pending = std::move(v.permissionRequests);
    v.permissionRequests.clear();
    std::vector<std::string> told;
    for (auto& entry : pending) {
        WKNotificationPermissionRequestDeny(entry.second);
        WKRelease(entry.second);
        bool already = false;
        for (const auto& origin : told)
            already = already || origin == entry.first;
        if (!already) {
            told.push_back(entry.first);
            v.sink->notifyPermissionGone(entry.first.c_str());
        }
    }
}

// ---- the owner's answers (main thread) ------------------------------------------------------

void notifyShown(uint64_t id)
{
    if (H.notifications)
        WKNotificationManagerProviderDidShowNotification(H.notifications, id);
}

void notifyClicked(uint64_t id)
{
    if (H.notifications)
        WKNotificationManagerProviderDidClickNotification(H.notifications, id);
}

void notifyClosed(uint64_t id)
{
    if (!H.notifications)
        return;
    WKUInt64Ref value = WKUInt64Create(id);
    WKTypeRef values[] = { value };
    WKArrayRef array = WKArrayCreate(values, 1);
    WKNotificationManagerProviderDidCloseNotifications(H.notifications, array);
    WKRelease(array);
    WKRelease(value);
    H.notificationViews.erase(id);
}

// answer: 1 allow, 0 block (both remembered), -1 dismiss (the pending requests are refused and
// nothing is remembered: the site may ask again).
void notifyPermission(const std::string& origin, int answer)
{
    // The policy first: the WebProcess handles its messages in order, and the page's
    // continuation (which makes `new Notification` look the permission up) must run after it.
    if (answer >= 0 && H.notifications && !origin.empty()) {
        WKSecurityOriginRef site = originFromString(origin);
        WKNotificationManagerProviderDidUpdateNotificationPolicy(H.notifications, site, answer > 0);
        WKRelease(site);
    }
    settle(origin, answer > 0);
}

// The person revoked the decision about `origin`: the pages of that site get "default" again.
void notifyForget(const std::string& origin)
{
    if (!H.notifications || origin.empty())
        return;
    WKSecurityOriginRef site = originFromString(origin);
    WKTypeRef values[] = { site };
    WKArrayRef array = WKArrayCreate(values, 1);
    WKNotificationManagerProviderDidRemoveNotificationPolicies(H.notifications, array);
    WKRelease(array);
    WKRelease(site);
}

} // namespace webkitapp
