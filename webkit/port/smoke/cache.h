// webkit-smoke's cache mode (issue #892): WEBKIT_SMOKE_CACHE=<dir> gives the page a website data
// store whose HTTP cache is <dir>/cache, and at the end counts the NetworkProcess's open descriptors.
// See cache.cpp.

#pragma once

#include <WebKit/WKBase.h>
#include <sys/types.h>

namespace smoke {

// The data store the cache mode asks for (the caller sets it on the page configuration), or null
// without WEBKIT_SMOKE_CACHE.
WKWebsiteDataStoreRef cacheStore();

// Prints WEBKIT:CACHE:FDS:total=<n>:cache=<m> for process `network` (m: those open on a file below
// the cache directory). Does nothing without the mode.
void reportCacheDescriptors(pid_t network);

} // namespace smoke
