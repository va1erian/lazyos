// webkit-smoke's cache mode (issue #892, cache.h).
//
// The curl backend's NetworkCache::Data once kept the descriptor of every file it mapped (patch 0019),
// so a NetworkProcess holding many cache entries mapped held as many descriptors, on the way to its
// limit. tools/webkit/cachecheck.py runs this mode twice on one cache directory against a page with
// many cacheable pictures (the second run reads them back from the disk cache) and judges the count:
//
//   WEBKIT:CACHE:DIR:<dir>
//   WEBKIT:CACHE:FDS:total=<n>:cache=<m>      the NetworkProcess's descriptors at the end of the run
//                                             (/proc/<pid>/fd), m of them open below <dir>/cache

#include "cache.h"

#include <WebKit/WKString.h>
#include <WebKit/WKType.h>
#include <WebKit/WKWebsiteDataStoreConfigurationRef.h>
#include <WebKit/WKWebsiteDataStoreRef.h>

#include <dirent.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string>
#include <sys/stat.h>
#include <unistd.h>

namespace smoke {
namespace {

std::string cacheDirectory()
{
    const char* value = getenv("WEBKIT_SMOKE_CACHE");
    return value && *value == '/' ? value : std::string();
}

void setPath(WKWebsiteDataStoreConfigurationRef configuration, void (*set)(WKWebsiteDataStoreConfigurationRef, WKStringRef),
    const std::string& path)
{
    WKStringRef value = WKStringCreateWithUTF8CString(path.c_str());
    set(configuration, value);
    WKRelease(value);
}

} // namespace

WKWebsiteDataStoreRef cacheStore()
{
    const std::string root = cacheDirectory();
    if (root.empty())
        return nullptr;
    mkdir(root.c_str(), 0700);
    mkdir((root + "/cache").c_str(), 0700);
    printf("WEBKIT:CACHE:DIR:%s\n", root.c_str());
    WKWebsiteDataStoreConfigurationRef configuration = WKWebsiteDataStoreConfigurationCreate();
    setPath(configuration, WKWebsiteDataStoreConfigurationSetNetworkCacheDirectory, root + "/cache");
    setPath(configuration, WKWebsiteDataStoreConfigurationSetCookieStorageFile, root + "/cookies.db");
    WKWebsiteDataStoreRef store = WKWebsiteDataStoreCreateWithConfiguration(configuration);
    WKRelease(configuration);
    return store;
}

void reportCacheDescriptors(pid_t network)
{
    const std::string root = cacheDirectory();
    if (root.empty() || network <= 0)
        return;
    const std::string cache = root + "/cache/";
    const std::string folder = "/proc/" + std::to_string(network) + "/fd";
    DIR* listing = opendir(folder.c_str());
    if (!listing) {
        printf("WEBKIT:CACHE:FDS:FAIL:cannot list %s\n", folder.c_str());
        return;
    }
    int total = 0, cached = 0;
    while (dirent* entry = readdir(listing)) {
        if (entry->d_name[0] == '.')
            continue;
        ++total;
        char target[PATH_MAX];
        const ssize_t length = readlink((folder + "/" + entry->d_name).c_str(), target, sizeof target - 1);
        if (length <= 0)
            continue;
        target[length] = 0;
        cached += std::string(target).compare(0, cache.size(), cache) == 0 ? 1 : 0;
    }
    closedir(listing);
    printf("WEBKIT:CACHE:FDS:total=%d:cache=%d\n", total, cached);
    fflush(stdout);
}

} // namespace smoke
