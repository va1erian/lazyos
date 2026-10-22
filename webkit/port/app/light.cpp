// Light views (docs/webd-plan.md 3.6, webkit/README.md "The engine"): a page whose content is the owner's
// HTML and nothing else. What makes a view light, all of it enforced here and none of it left to the page or
// to the owner remembering:
//
//  * No network. A content rule list, compiled once for the process, blocks every load except data:, blob:
//    and about: URLs (subresources of every kind: images, style sheets and their @imports, scripts, frames,
//    fetch, XMLHttpRequest, WebSocket, preconnects); and the navigation policy loads nothing that is not the
//    owner's own loadHtml, so a link, a form and a redirect cannot start a load either. The rule list is a
//    separate mechanism from the policy on purpose: the policy sees navigations only, the list sees requests.
//  * Navigation locked. A main-frame navigation the owner did not ask for is refused and reported to the
//    sink (ViewSink::linkClicked) with its absolute URL. A move inside the page (a #fragment link) is the
//    page's own and is allowed.
//  * Scripts as the owner says (WKPreferences JavaScript), and no storage, windows or beacons even when on.
//  * Nothing of a browser: createView forces the view's features to 0, so no UI client, dialogs, downloads,
//    notifications, blocking or navigation callbacks to the owner are installed.
//
// The data store is the non-persistent one engine.cpp makes for a view with no profile directory.

#include <WebKit/WKData.h>
#include <WebKit/WKFrameInfoRef.h>
#include <WebKit/WKFramePolicyListener.h>
#include <WebKit/WKNavigationActionRef.h>
#include <WebKit/WKNavigationResponseRef.h>
#include <WebKit/WKPage.h>
#include <WebKit/WKPageConfigurationRef.h>
#include <WebKit/WKPagePrivate.h>
#include <WebKit/WKPreferencesRef.h>
#include <WebKit/WKPreferencesRefPrivate.h>
#include <WebKit/WKString.h>
#include <WebKit/WKType.h>
#include <WebKit/WKURL.h>
#include <WebKit/WKURLRequest.h>
#include <WebKit/WKUserContentControllerRef.h>
#include <WebKit/WKUserContentExtensionStoreRef.h>

#include <ctype.h>
#include <errno.h>
#include <stdlib.h>
#include <string>
#include <sys/stat.h>
#include <unistd.h>
#include <utility>
#include <vector>

#include "engine.h"

namespace webkitapp {

namespace {

// Block everything, then take the block back for the URLs that cannot reach a network. WebKit's url-filter
// has no alternation, so one rule per scheme.
constexpr const char* kLockRules = R"([
 {"trigger":{"url-filter":".*"},"action":{"type":"block"}},
 {"trigger":{"url-filter":"^data:"},"action":{"type":"ignore-previous-rules"}},
 {"trigger":{"url-filter":"^blob:"},"action":{"type":"ignore-previous-rules"}},
 {"trigger":{"url-filter":"^about:"},"action":{"type":"ignore-previous-rules"}}
])";
constexpr const char* kLockId = "webd-light-lock";

LightLock lockState = LightLock::Pending;
bool lockStarted = false;
WKUserContentExtensionStoreRef lockStore = nullptr;
WKUserContentFilterRef lockFilter = nullptr;
std::vector<std::function<void()>> lockWaiters;

void settleLock(LightLock state)
{
    lockState = state;
    auto waiters = std::move(lockWaiters);
    lockWaiters.clear();
    for (auto& task : waiters)
        task();
}

void lockCompiled(WKUserContentFilterRef filter, WKUserContentExtensionStoreResult result, void*)
{
    if (result == kWKUserContentExtensionStoreSuccess && filter) {
        // The callback's reference is ours now; every light view's controller takes the list.
        lockFilter = filter;
        say("LIGHT:LOCK:READY");
        settleLock(LightLock::Ready);
        return;
    }
    if (filter)
        WKRelease(filter);
    say("FAIL:light:the network lock did not compile (result %u)", static_cast<unsigned>(result));
    settleLock(LightLock::Failed);
}

bool makeDirectories(const std::string& path)
{
    for (size_t at = 1; at <= path.size(); ++at) {
        if (at != path.size() && path[at] != '/')
            continue;
        if (mkdir(path.substr(0, at).c_str(), 0700) != 0 && errno != EEXIST)
            return false;
    }
    return true;
}

std::string lower(std::string text)
{
    for (char& c : text)
        c = static_cast<char>(tolower(static_cast<unsigned char>(c)));
    return text;
}

std::string withoutFragment(const std::string& address)
{
    const size_t at = address.find('#');
    return at == std::string::npos ? address : address.substr(0, at);
}

// The schemes that cannot reach a network: what a frame of the page may load.
bool inertScheme(const std::string& address)
{
    const std::string text = lower(address.substr(0, 8));
    return text.compare(0, 5, "data:") == 0 || text.compare(0, 5, "blob:") == 0 || text.compare(0, 6, "about:") == 0;
}

// A base URL that makes the page's origin opaque and its relative URLs useless: about:blank or a data: URL.
bool allowedBase(const std::string& normalised)
{
    return normalised == "about:blank" || lower(normalised.substr(0, 5)) == "data:";
}

// Extra CSS goes after the page's own content (a <style> there is part of the body, and wins over the page's
// rules of the same weight, as a user's style should). Text that could close the element is not CSS.
bool safeStyle(const std::string& css) { return css.find("</") == std::string::npos && css.find("<!") == std::string::npos; }

} // namespace

void prepareLightLock()
{
    if (lockStarted)
        return;
    lockStarted = true;
    // The folder is the owner's to name (webd: a folder of its own profile root, writable by its label). No
    // fallback: a predictable folder in a shared place is not one a lock may rest on, so without it the
    // light views are refused (fail closed).
    const char* chosen = getenv("LAZYOS_WEBD_RULES_DIR");
    if (!chosen || *chosen != '/') {
        say("FAIL:light:LAZYOS_WEBD_RULES_DIR names no folder for the rule store");
        settleLock(LightLock::Failed);
        return;
    }
    const std::string directory = chosen;
    if (!makeDirectories(directory)) {
        say("FAIL:light:cannot make the rule store folder");
        settleLock(LightLock::Failed);
        return;
    }
    WKStringRef path = WKStringCreateWithUTF8CString(directory.c_str());
    lockStore = WKUserContentExtensionStoreCreate(path);
    WKRelease(path);
    if (!lockStore) {
        say("FAIL:light:this WebKit has no content extensions");
        settleLock(LightLock::Failed);
        return;
    }
    WKStringRef id = WKStringCreateWithUTF8CString(kLockId);
    WKStringRef rules = WKStringCreateWithUTF8CString(kLockRules);
    WKUserContentExtensionStoreCompile(lockStore, id, rules, nullptr, lockCompiled);
    WKRelease(rules);
    WKRelease(id);
}

LightLock lightLock() { return lockState; }

WKUserContentFilterRef lightLockFilter() { return lockState == LightLock::Ready ? lockFilter : nullptr; }

void whenLightLockSettled(std::function<void()> task)
{
    if (lockState != LightLock::Pending)
        task();
    else
        lockWaiters.push_back(std::move(task));
}

bool configureLight(View& view, WKPageConfigurationRef configuration)
{
    if (lockState != LightLock::Ready || !lockFilter)
        return false;
    WKPreferencesRef preferences = WKPreferencesCreate();
    WKPreferencesSetJavaScriptEnabled(preferences, view.config.scripts);
    // Whatever script there is gets no windows, storage or beacons (the lock stops its requests).
    WKPreferencesSetJavaScriptCanOpenWindowsAutomatically(preferences, false);
    WKPreferencesSetLocalStorageEnabled(preferences, false);
    WKPreferencesSetStorageBlockingPolicy(preferences, kWKBlockAllStorage);
    WKPreferencesSetBeaconAPIEnabled(preferences, false);
    if (view.config.defaultFontSize > 0) {
        WKPreferencesSetDefaultFontSize(preferences, static_cast<uint32_t>(view.config.defaultFontSize));
        WKPreferencesSetMinimumFontSize(preferences, static_cast<uint32_t>(view.config.defaultFontSize));
    }
    WKPageConfigurationSetPreferences(configuration, preferences);
    WKRelease(preferences);
    // prefers-color-scheme is compiled out of this WebKit (ENABLE_DARK_MODE_CSS is off in the port, and the C
    // API's only switch, WKPageSetUseDarkAppearanceForTesting, changes nothing without it): said, not failed.
    // An owner that wants a dark page passes a user_style.
    if (view.config.colorScheme == 1)
        say("CONFIG:IGNORED:color_scheme");

    // The lock list on a controller of this page alone, before the page exists: no request is made before it.
    WKUserContentControllerRef controller = WKUserContentControllerCreate();
    WKUserContentControllerAddUserContentFilter(controller, lockFilter);
    WKPageConfigurationSetUserContentController(configuration, controller);
    return true;
}

void lightNavigationAction(View& v, WKNavigationActionRef action, WKFramePolicyListenerRef listener)
{
    const WKFrameNavigationType type = WKNavigationActionGetNavigationType(action);
    WKURLRequestRef request = WKNavigationActionCopyRequest(action);
    WKURLRef wkUrl = request ? WKURLRequestCopyURL(request) : nullptr;
    const std::string address = utf8(wkUrl);
    if (wkUrl)
        WKRelease(wkUrl);
    if (request)
        WKRelease(request);
    WKFrameInfoRef target = WKNavigationActionCopyTargetFrameInfo(action);
    const bool mainFrame = target && WKFrameInfoGetIsMainFrame(target);
    const bool frame = target && !mainFrame;
    if (target)
        WKRelease(target);

    // A frame of the page: only what cannot reach a network (an srcdoc is about:srcdoc).
    if (frame) {
        if (inertScheme(address)) {
            WKFramePolicyListenerUse(listener);
        } else {
            say("LIGHT:REFUSED:frame");
            WKFramePolicyListenerIgnore(listener);
        }
        return;
    }

    const std::string page = withoutFragment(address);
    if (mainFrame && type == kWKFrameNavigationTypeOther && v.lightLoads > 0 && inertScheme(address)) {
        // The owner's loadHtml: the one navigation of a light view. WebKit names the base URL in its canonical
        // form (the scheme in lower case, spaces and non-ASCII escaped), which is not the owner's spelling, so
        // the load is recognised by being owed and inert; the fragments are compared against what it became.
        --v.lightLoads;
        v.lightBase = page;
        WKFramePolicyListenerUse(listener);
        return;
    }
    if (mainFrame && page == v.lightBase && page != address
        && (type == kWKFrameNavigationTypeLinkClicked || type == kWKFrameNavigationTypeOther)) {
        // A #fragment of the page itself: it only scrolls.
        WKFramePolicyListenerUse(listener);
        return;
    }

    WKFramePolicyListenerIgnore(listener);
    if (type == kWKFrameNavigationTypeBackForward || type == kWKFrameNavigationTypeReload) {
        say("LIGHT:REFUSED:history");
        return;
    }
    // A link, a form, a meta refresh, a script's location, a target=_blank: the owner decides what it means.
    say("LIGHT:LINK");
    v.sink->linkClicked(address.c_str());
}

void lightNavigationResponse(View&, WKNavigationResponseRef response, WKFramePolicyListenerRef listener)
{
    // Nothing is a download here: a response the page cannot show is not shown.
    if (WKNavigationResponseCanShowMIMEType(response))
        WKFramePolicyListenerUse(listener);
    else
        WKFramePolicyListenerIgnore(listener);
}

void View::loadHtml(const std::string& html, const std::string& baseUrl)
{
    loadRequested = true;
    WKURLRef base = WKURLCreateWithUTF8CString(baseUrl.empty() ? "about:blank" : baseUrl.c_str());
    const std::string normal = utf8(base);
    if (!allowedBase(normal)) {
        WKRelease(base);
        say("LIGHT:BASE:REFUSED");
        error = "The page was not loaded: its base URL must be about:blank or a data: URL.";
        ++stateSerial;
        postState(*this);
        return;
    }
    std::string document = html;
    if (!config.userStyle.empty()) {
        if (safeStyle(config.userStyle))
            document += "\n<style>" + config.userStyle + "</style>";
        else
            say("CONFIG:IGNORED:user_style");
    }
    lightBase = withoutFragment(normal);
    ++lightLoads;
    WKDataRef data = WKDataCreate(reinterpret_cast<const unsigned char*>(document.data()), document.size());
    WKStringRef mime = WKStringCreateWithUTF8CString("text/html");
    WKStringRef encoding = WKStringCreateWithUTF8CString("UTF-8");
    WKPageLoadData(page, data, mime, encoding, base);
    WKRelease(encoding);
    WKRelease(mime);
    WKRelease(data);
    WKRelease(base);
}

} // namespace webkitapp
