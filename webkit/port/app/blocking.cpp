// Content blocking (issue #845, docs/lazywebplus.md): WebKit's content-extension rule lists, compiled
// into a store below the profile directory and attached to the page's user content controller.
//
// The owner holds the lists (the window converts a filter list to WebKit's JSON,
// `xui-app/crates/adblock`) and the policy (its switch, its per-site exceptions); this file only holds
// them for the engine:
//
//  * blockingLoad / blockingCompile put a list in the profile's store and on the profile's pages, and
//    say how it went;
//  * every main-frame navigation asks ViewSink::blocking and attaches the lists or takes them off,
//    before the load starts, so a site the user excepted loads whole and the next one is blocked again;
//  * a load a list blocked comes back as a count (patch 0018 makes the C API report it).
//
// The store and the compiled lists belong to the Profile (the views of one profile directory share
// them); the controller, what the last navigation decided and the count belong to the View.
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
// What patch 0018 reports for a load a rule list blocked.
constexpr const char* kBlockedNotification = "lazyos:blocked";

// A compile or lookup in flight: the callback gets it back as its context. The profile is held weakly
// (the last view of it may be gone by the time the store answers) and the view only to tell it.
struct Job {
    std::weak_ptr<Profile> profile;
    int view { 0 };
    std::string id;
    int generation { 0 };
};

// Puts the lists on the page, or takes them off, to match what the last navigation decided. A
// change of the lists themselves rebuilds the attachment; a navigation that decides as the last one
// did leaves it alone (each rebuild is a message to every process that uses the controller).
void reapply(View& v, bool listsChanged)
{
    if (!v.controller)
        return;
    const bool want = v.blockWanted && !v.profile->lists.empty();
    if (!listsChanged && want == v.blockAttached)
        return;
    // Rebuilt in full: a list that was replaced or forgotten must not stay attached.
    WKUserContentControllerRemoveAllUserContentFilters(v.controller);
    if (want) {
        for (const auto& entry : v.profile->lists)
            WKUserContentControllerAddUserContentFilter(v.controller, static_cast<WKUserContentFilterRef>(entry.second));
    }
    v.blockAttached = want;
}

// The lists of a profile changed: every page of it takes the new set.
void reapplyAll(const Profile& profile)
{
    for (auto& entry : H.views) {
        if (entry.second->profile.get() == &profile)
            reapply(*entry.second, true);
    }
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

void report(int view, const std::string& id, int status)
{
    say("BLOCK:LIST:%s:%d", oneLine(id).c_str(), status);
    if (View* v = viewById(view))
        v->sink->blockingList(id.c_str(), status);
}

// The store's answer for a compile or a lookup.
void listReady(WKUserContentFilterRef filter, WKUserContentExtensionStoreResult result, void* context)
{
    std::unique_ptr<Job> job(static_cast<Job*>(context));
    std::shared_ptr<Profile> profile = job->profile.lock();
    if (!profile || job->generation != profile->generation[job->id]) {
        // The list was forgotten while this was under way (or its profile is gone): it must not come back.
        if (filter)
            WKRelease(filter);
        return;
    }
    int status = statusFor(result);
    if (status == LZW_BLOCK_READY && !filter)
        status = LZW_BLOCK_COMPILE_FAILED;
    if (status == LZW_BLOCK_READY) {
        auto old = profile->lists.find(job->id);
        if (old != profile->lists.end())
            WKRelease(old->second);
        // The callback's reference is ours now.
        profile->lists[job->id] = filter;
        reapplyAll(*profile);
    }
    report(job->view, job->id, status);
}

void listRemoved(WKUserContentFilterRef, WKUserContentExtensionStoreResult, void* context)
{
    delete static_cast<Job*>(context);
}

} // namespace

void startBlocking(View& v, WKPageConfigurationRef configuration)
{
    if (!v.has(LZW_FEATURE_BLOCKING))
        return;
    // The controller goes in the page's configuration before the view exists; lists come later.
    v.controller = WKUserContentControllerCreate();
    WKPageConfigurationSetUserContentController(configuration, v.controller);
    Profile& profile = *v.profile;
    if (profile.rules)
        return;
    if (profile.directory.empty()) {
        say("BLOCK:NONE:no profile directory");
        return;
    }
    const std::string folder = profile.directory + "/" + kStoreFolder;
    WKStringRef path = WKStringCreateWithUTF8CString(folder.c_str());
    profile.rules = WKUserContentExtensionStoreCreate(path);
    WKRelease(path);
    if (!profile.rules)
        say("BLOCK:NONE:this WebKit has no content extensions");
    else
        say("BLOCK:UP");
}

void blockingNavigation(View& v, const std::string& address, bool mainFrame)
{
    if (!v.controller || !mainFrame)
        return;
    v.blockWanted = v.sink->blocking(address.c_str()) != 0;
    reapply(v, false);
}

void blockingSettle(View& v)
{
    if (!v.controller)
        return;
    blockingNavigation(v, committedUrl(v), true);
}

void blockingCommitted(View& v)
{
    if (!v.controller)
        return;
    v.blockedCount = 0;
    v.sink->blocked(0);
}

void blockingFinished(View& v)
{
    if (v.controller && v.blockedCount > 0)
        say("BLOCK:COUNT:%d", v.blockedCount);
}

void contentRuleListNotification(WKPageRef, WKURLRef, WKArrayRef, WKArrayRef notifications, const void* clientInfo)
{
    View& v = *viewOf(clientInfo);
    if (!v.controller || !notifications)
        return;
    int added = 0;
    for (size_t i = 0, n = WKArrayGetSize(notifications); i < n; ++i) {
        WKTypeRef item = WKArrayGetItemAtIndex(notifications, i);
        if (item && WKGetTypeID(item) == WKStringGetTypeID()
            && WKStringIsEqualToUTF8CString(static_cast<WKStringRef>(item), kBlockedNotification))
            ++added;
    }
    if (added) {
        v.blockedCount += added;
        v.sink->blocked(v.blockedCount);
    }
}

void blockingLoad(View& v, const std::string& name)
{
    Profile& profile = *v.profile;
    if (!profile.rules) {
        report(v.id, name, LZW_BLOCK_UNAVAILABLE);
        return;
    }
    WKStringRef wkId = WKStringCreateWithUTF8CString(name.c_str());
    WKUserContentExtensionStoreLookup(profile.rules, wkId, new Job { v.profile, v.id, name, profile.generation[name] }, listReady);
    WKRelease(wkId);
}

void blockingCompile(View& v, const std::string& name, const std::string& source)
{
    Profile& profile = *v.profile;
    if (!profile.rules) {
        report(v.id, name, LZW_BLOCK_UNAVAILABLE);
        return;
    }
    WKStringRef wkId = WKStringCreateWithUTF8CString(name.c_str());
    WKStringRef wkJson = WKStringCreateWithUTF8CString(source.c_str());
    WKUserContentExtensionStoreCompile(profile.rules, wkId, wkJson, new Job { v.profile, v.id, name, profile.generation[name] }, listReady);
    WKRelease(wkJson);
    WKRelease(wkId);
}

void blockingForget(View& v, const std::string& name)
{
    Profile& profile = *v.profile;
    ++profile.generation[name];
    auto it = profile.lists.find(name);
    if (it != profile.lists.end()) {
        WKRelease(it->second);
        profile.lists.erase(it);
        reapplyAll(profile);
    }
    if (!profile.rules)
        return;
    WKStringRef wkId = WKStringCreateWithUTF8CString(name.c_str());
    WKUserContentExtensionStoreRemove(profile.rules, wkId, new Job { v.profile, v.id, name, 0 }, listRemoved);
    WKRelease(wkId);
}

} // namespace webkitapp
