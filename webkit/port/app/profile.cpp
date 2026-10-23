// Profiles: the website data stores views live in, and the environment the engine processes inherit
// (docs/lazyweb.md; docs/webd-plan.md 3.3).
//
// A persistent profile is a directory: WebKit keeps everything of its pages (cookies, the HTTP cache, local
// storage, IndexedDB, ...) below it, in a data store of its own, so two directories are two stores and two
// NetworkProcess sessions. The children's environment is the other half: the launcher starts them in
// LAZYOS_WEBKIT_PROFILE (WebKit's default data directories follow it, patch 0015), and curl opens a default
// cookie database at CURL_COOKIE_JAR_PATH (the working directory otherwise) for every session before a
// store's own file replaces it (NetworkStorageSessionCurl.cpp, NetworkSessionCurl.cpp). A process takes its
// environment when it starts and one NetworkProcess serves every store, so:
//
//  * a host with one profile (LazyWeb) names the first persistent profile's directory and jar, as it always did;
//  * a host of several (webd, fixChildrenEnvironment) decides it once at start: the children's folder, and an
//    in-memory default database. Each persistent store's session then opens its own `cookieStorageFile`, an
//    ephemeral one keeps its cookies in a SQLite database of its own in memory, and no view changes either.

#include <WebKit/WKString.h>
#include <WebKit/WKType.h>
#include <WebKit/WKWebsiteDataStoreConfigurationRef.h>
#include <WebKit/WKWebsiteDataStoreRef.h>

#include <errno.h>
#include <stdlib.h>
#include <string>
#include <sys/stat.h>

#include "engine.h"

namespace webkitapp {

namespace {

// The cookie jar, below the profile directory (a SQLite database the NetworkProcess owns).
constexpr const char* kCookieJar = "cookies/cookie.jar.db";

// The website data store the page lives in: everything WebKit keeps below `profile`; WebKit's own defaults
// for the directories this does not name follow the children's folder (patch 0015). No profile gives an
// ephemeral store, never a directory of the working directory.
WKWebsiteDataStoreRef createDataStore(const std::string& profile, bool memoryOnly)
{
    // The owner asked for the page's data to stay in memory (issue #940): the profile still holds the
    // content-blocking store (startBlocking), but nothing of the page is written to it. With one profile the
    // children still start in it (the one folder their rules let them write) and curl's default session keeps
    // its cookie database in SQLite's memory (`CookieJarDB::isOnMemory`), not in the working directory; a
    // host whose environment is fixed has both already.
    if (memoryOnly) {
        if (!H.environmentFixed) {
            if (!profile.empty() && makePrivateDirectories(profile))
                setenv("LAZYOS_WEBKIT_PROFILE", profile.c_str(), 1);
            setenv("CURL_COOKIE_JAR_PATH", ":memory:", 1);
        } else if (!profile.empty()) {
            makePrivateDirectories(profile);
        }
        say("PROFILE:EPHEMERAL:the page's data is kept in memory");
        return WKWebsiteDataStoreCreateNonPersistentDataStore();
    }
    if (profile.empty() || !makePrivateDirectories(profile)) {
        say("PROFILE:NONE:the browser keeps nothing");
        return WKWebsiteDataStoreCreateNonPersistentDataStore();
    }
    exportProfileEnvironment(profile);

    auto path = [&profile](const char* leaf) {
        return WKStringCreateWithUTF8CString((profile + "/" + leaf).c_str());
    };
    WKWebsiteDataStoreConfigurationRef configuration = WKWebsiteDataStoreConfigurationCreate();
    struct Setting {
        void (*set)(WKWebsiteDataStoreConfigurationRef, WKStringRef);
        const char* leaf;
    };
    const Setting settings[] = {
        { WKWebsiteDataStoreConfigurationSetCookieStorageFile, kCookieJar },
        { WKWebsiteDataStoreConfigurationSetNetworkCacheDirectory, "cache" },
        { WKWebsiteDataStoreConfigurationSetLocalStorageDirectory, "storage/local" },
        { WKWebsiteDataStoreConfigurationSetIndexedDBDatabaseDirectory, "storage/indexeddb" },
        { WKWebsiteDataStoreConfigurationSetWebSQLDatabaseDirectory, "storage/websql" },
        { WKWebsiteDataStoreConfigurationSetCacheStorageDirectory, "storage/cachestorage" },
        { WKWebsiteDataStoreConfigurationSetGeneralStorageDirectory, "storage/general" },
        { WKWebsiteDataStoreConfigurationSetResourceLoadStatisticsDirectory, "storage/statistics" },
        { WKWebsiteDataStoreConfigurationSetServiceWorkerRegistrationDirectory, "storage/serviceworkers" },
    };
    for (const auto& setting : settings) {
        WKStringRef value = path(setting.leaf);
        setting.set(configuration, value);
        WKRelease(value);
    }
    // The directories exist before a process that may only write inside them looks for one.
    for (const char* leaf : { "cookies", "cache", "storage" })
        makePrivateDirectories(profile + "/" + leaf);
    WKWebsiteDataStoreRef store = WKWebsiteDataStoreCreateWithConfiguration(configuration);
    WKRelease(configuration);
    say("PROFILE:PASS");
    sayHarness("PROFILE:DIR:%s", oneLine(profile).c_str());
    return store;
}

} // namespace

Profile::~Profile()
{
    for (auto& list : lists)
        WKRelease(list.second);
    if (rules)
        WKRelease(rules);
    if (store)
        WKRelease(store);
}

bool makePrivateDirectories(const std::string& path)
{
    for (size_t at = 1; at <= path.size(); ++at) {
        if (at != path.size() && path[at] != '/')
            continue;
        const std::string prefix = path.substr(0, at);
        if (mkdir(prefix.c_str(), 0700) != 0 && errno != EEXIST)
            return false;
    }
    return true;
}

void exportProfileEnvironment(const std::string& profile)
{
    // A host of several profiles decided this at start; the first persistent profile's jar would be the
    // default session's of every other one.
    if (H.environmentFixed)
        return;
    static bool exported = false;
    if (exported)
        return;
    exported = true;
    setenv("LAZYOS_WEBKIT_PROFILE", profile.c_str(), 1);
    setenv("CURL_COOKIE_JAR_PATH", (profile + "/" + kCookieJar).c_str(), 1);
}

bool fixChildrenEnvironment(const std::string& directory)
{
    if (directory.empty() || directory[0] != '/' || !makePrivateDirectories(directory)) {
        say("FAIL:host:cannot make the children's folder");
        return false;
    }
    setenv("LAZYOS_WEBKIT_PROFILE", directory.c_str(), 1);
    setenv("CURL_COOKIE_JAR_PATH", ":memory:", 1);
    H.environmentFixed = true;
    say("HOST:CHILDREN:FIXED");
    return true;
}

std::shared_ptr<Profile> profileFor(const std::string& directory, bool memoryOnly)
{
    // A directory used both ways is two profiles: one persistent store, one in memory.
    const std::string key = directory + (memoryOnly ? "\n:memory" : "");
    if (!directory.empty()) {
        auto known = H.profiles.find(key);
        if (known != H.profiles.end()) {
            if (std::shared_ptr<Profile> shared = known->second.lock())
                return shared;
        }
    }
    auto profile = std::make_shared<Profile>();
    profile->directory = directory;
    profile->store = createDataStore(directory, memoryOnly);
    if (!directory.empty()) {
        H.profiles[key] = profile;
        // A persistent data store lives as long as the process: releasing the last one while its
        // NetworkProcess session is open aborted the UI process (webkit-views two), and the old
        // single-page host never let go of its store either.
        H.keptProfiles.push_back(profile);
    }
    return profile;
}

} // namespace webkitapp
