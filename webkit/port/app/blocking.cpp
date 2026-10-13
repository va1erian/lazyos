// Content blocking (issue #845, docs/lazywebplus.md): WebKit's content-extension rule lists, compiled
// into a store below the profile directory and attached to the page's user content controller.
//
// The window owns the lists (it converts a filter list to WebKit's JSON, `xui-app/crates/adblock`) and
// the policy (its switch, its per-site exceptions); this file only holds them for the engine:
//
//  * lzw_engine_blocking_load / _compile put a list in the store and on the page, and say how it went;
//  * every main-frame navigation asks lzw_ui_blocking and attaches the lists or takes them off, before
//    the load starts, so a site the user excepted loads whole and the next one is blocked again;
//  * a load a list blocked comes back as a count (patch 0017 makes the C API report it).
//
// Compiling happens on WebKit's work queue and the callbacks come back on the run loop; nothing here
// blocks it.

#include "engine.h"
#include "lazyos_ui.h"

#include <WebKit/WKArray.h>
#include <WebKit/WKPageConfigurationRef.h>
#include <WebKit/WKPageNavigationClient.h>
#include <WebKit/WKString.h>
#include <WebKit/WKType.h>
#include <WebKit/WKUserContentControllerRef.h>
#include <WebKit/WKUserContentExtensionStoreRef.h>

#include <map>
#include <memory>
#include <string>
#include <utility>

namespace webkitapp {

namespace {

// The compiled lists live in this folder of the profile.
constexpr const char* kStoreFolder = "contentextensions";
// What patch 0017 reports for a load a rule list blocked.
constexpr const char* kBlockedNotification = "lazyos:blocked";

struct Blocking {
    WKUserContentControllerRef controller { nullptr };
    WKUserContentExtensionStoreRef store { nullptr };
    // The compiled lists that are ready, by id: each holds the reference the store's callback gave.
    std::map<std::string, WKTypeRef> lists;
    // What the last main-frame navigation decided, and whether the lists are on the page now.
    bool wanted { true };
    bool attached { false };
    int blocked { 0 };
    // Bumped when a list is forgotten, so a compile or lookup still in flight for it is dropped.
    std::map<std::string, int> generation;
};

Blocking B;

// A compile or lookup in flight: the callback gets it back as its context.
struct Job {
    std::string id;
    int generation { 0 };
};

// Puts the lists on the page, or takes them off, to match what the last navigation decided. A
// change of the lists themselves rebuilds the attachment; a navigation that decides as the last one
// did leaves it alone (each rebuild is a message to every process that uses the controller).
void reapply(bool listsChanged)
{
    if (!B.controller)
        return;
    const bool want = B.wanted && !B.lists.empty();
    if (!listsChanged && want == B.attached)
        return;
    // Rebuilt in full: a list that was replaced or forgotten must not stay attached.
    WKUserContentControllerRemoveAllUserContentFilters(B.controller);
    if (want) {
        for (const auto& entry : B.lists)
            WKUserContentControllerAddUserContentFilter(B.controller, static_cast<WKUserContentFilterRef>(entry.second));
    }
    B.attached = want;
}

int statusFor(WKUserContentExtensionStoreResult result)
{
    switch (result) {
    case kWKUserContentExtensionStoreSuccess:
        return LZW_BLOCK_READY;
    case kWKUserContentExtensionStoreLookupFailed:
        return LZW_BLOCK_NOT_STORED;
    case kWKUserContentExtensionStoreVersionMismatch:
        return LZW_BLOCK_STALE;
    default:
        return LZW_BLOCK_COMPILE_FAILED;
    }
}

void report(const std::string& id, int status)
{
    say("BLOCK:LIST:%s:%d", oneLine(id).c_str(), status);
    lzw_ui_blocking_list(id.c_str(), status);
}

// The store's answer for a compile or a lookup.
void listReady(WKUserContentFilterRef filter, WKUserContentExtensionStoreResult result, void* context)
{
    std::unique_ptr<Job> job(static_cast<Job*>(context));
    if (job->generation != B.generation[job->id]) {
        // The list was forgotten while this was under way: it must not come back.
        if (filter)
            WKRelease(filter);
        return;
    }
    int status = statusFor(result);
    if (status == LZW_BLOCK_READY && !filter)
        status = LZW_BLOCK_COMPILE_FAILED;
    if (status == LZW_BLOCK_READY) {
        auto old = B.lists.find(job->id);
        if (old != B.lists.end())
            WKRelease(old->second);
        // The callback's reference is ours now.
        B.lists[job->id] = filter;
        reapply(true);
    }
    report(job->id, status);
}

void listRemoved(WKUserContentFilterRef, WKUserContentExtensionStoreResult, void* context)
{
    delete static_cast<Job*>(context);
}

} // namespace

void startBlocking(WKPageConfigurationRef configuration, const std::string& profile)
{
    if (!has(LZW_FEATURE_BLOCKING))
        return;
    // The controller goes in the page's configuration before the view exists; lists come later.
    B.controller = WKUserContentControllerCreate();
    WKPageConfigurationSetUserContentController(configuration, B.controller);
    if (profile.empty()) {
        say("BLOCK:NONE:no profile directory");
        return;
    }
    const std::string folder = profile + "/" + kStoreFolder;
    WKStringRef path = WKStringCreateWithUTF8CString(folder.c_str());
    B.store = WKUserContentExtensionStoreCreate(path);
    WKRelease(path);
    if (!B.store)
        say("BLOCK:NONE:this WebKit has no content extensions");
    else
        say("BLOCK:UP");
}

void blockingNavigation(const std::string& address, bool mainFrame)
{
    if (!B.controller || !mainFrame)
        return;
    B.wanted = lzw_ui_blocking(address.c_str()) != 0;
    reapply(false);
}

void blockingSettle()
{
    if (!B.controller)
        return;
    blockingNavigation(committedUrl(), true);
}

void blockingCommitted()
{
    if (!B.controller)
        return;
    B.blocked = 0;
    lzw_ui_blocked(0);
}

void blockingFinished()
{
    if (B.controller && B.blocked > 0)
        say("BLOCK:COUNT:%d", B.blocked);
}

void contentRuleListNotification(WKPageRef, WKURLRef, WKArrayRef, WKArrayRef notifications, const void*)
{
    if (!B.controller || !notifications)
        return;
    int added = 0;
    for (size_t i = 0, n = WKArrayGetSize(notifications); i < n; ++i) {
        WKTypeRef item = WKArrayGetItemAtIndex(notifications, i);
        if (item && WKGetTypeID(item) == WKStringGetTypeID()
            && WKStringIsEqualToUTF8CString(static_cast<WKStringRef>(item), kBlockedNotification))
            ++added;
    }
    if (added) {
        B.blocked += added;
        lzw_ui_blocked(B.blocked);
    }
}

} // namespace webkitapp

using namespace webkitapp;

extern "C" {

void lzw_engine_blocking_load(const char* id)
{
    std::string name = id ? id : "";
    onMain([name] {
        if (!B.store) {
            report(name, LZW_BLOCK_UNAVAILABLE);
            return;
        }
        WKStringRef wkId = WKStringCreateWithUTF8CString(name.c_str());
        WKUserContentExtensionStoreLookup(B.store, wkId, new Job { name, B.generation[name] }, listReady);
        WKRelease(wkId);
    });
}

void lzw_engine_blocking_compile(const char* id, const char* json)
{
    std::string name = id ? id : "";
    std::string source = json ? json : "";
    onMain([name, source = std::move(source)] {
        if (!B.store) {
            report(name, LZW_BLOCK_UNAVAILABLE);
            return;
        }
        WKStringRef wkId = WKStringCreateWithUTF8CString(name.c_str());
        WKStringRef wkJson = WKStringCreateWithUTF8CString(source.c_str());
        WKUserContentExtensionStoreCompile(B.store, wkId, wkJson, new Job { name, B.generation[name] }, listReady);
        WKRelease(wkJson);
        WKRelease(wkId);
    });
}

void lzw_engine_blocking_forget(const char* id)
{
    std::string name = id ? id : "";
    onMain([name] {
        ++B.generation[name];
        auto it = B.lists.find(name);
        if (it != B.lists.end()) {
            WKRelease(it->second);
            B.lists.erase(it);
            reapply(true);
        }
        if (!B.store)
            return;
        WKStringRef wkId = WKStringCreateWithUTF8CString(name.c_str());
        WKUserContentExtensionStoreRemove(B.store, wkId, new Job { name, 0 }, listRemoved);
        WKRelease(wkId);
    });
}

} // extern "C"
