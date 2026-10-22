// webd's engine half for FULL views (webd_abi.h, P2 slice 3): the host start that fixes the children's
// environment, a full view's configuration, and the calls only a full view takes (navigation, downloads,
// dialogs, held navigations, notifications, content blocking). Each copies its arguments in the caller's thread
// and queues the work for WebKit's main thread (slots.h); a call about a light view does nothing.

#include <WebKit/WKPage.h>

#include <algorithm>
#include <stddef.h>
#include <string.h>
#include <string>

#include "engine.h"
#include "slots.h"
#include "webd_abi.h"

namespace webkitapp {
namespace webd {

namespace {

// The features a full view may ask for: no host-wide ones (LZW_FEATURE_OWN_MARKERS, LZW_FEATURE_PROFILE).
constexpr int kViewFeatures = WEBD_FEATURE_PAGE_UI | WEBD_FEATURE_DIALOGS | WEBD_FEATURE_DOWNLOADS | WEBD_FEATURE_POLICY
    | WEBD_FEATURE_NOTIFICATIONS | WEBD_FEATURE_BLOCKING;
constexpr size_t kMaxPath = 4096;
constexpr size_t kMaxSuffix = 64;

// Whether the caller's struct reaches the end of `field` (at `offset`, `size` bytes).
bool covers(const webd_view_config& config, size_t offset, size_t size) { return config.struct_size >= offset + size; }

// A profile directory as the engine accepts it: absolute, no "." or ".." component, no empty one (trailing
// slashes are dropped). The service derives it from the caller's label; this only refuses what must never be.
bool cleanProfile(const char* text, std::string& out)
{
    const size_t length = strnlen(text, kMaxPath + 1);
    if (length > kMaxPath || text[0] != '/')
        return false;
    std::string path(text, length);
    while (path.size() > 1 && path.back() == '/')
        path.pop_back();
    size_t start = 1;
    while (start <= path.size()) {
        const size_t end = std::min(path.find('/', start), path.size());
        const std::string part = path.substr(start, end - start);
        // ("/" alone is refused too: its one component is empty.)
        if (part.empty() || part == "." || part == "..")
            return false;
        start = end + 1;
    }
    out = path;
    return true;
}

// A suffix that can only be text in one header line: printable ASCII, at most 64 bytes.
bool cleanSuffix(const char* text, std::string& out)
{
    const size_t length = strnlen(text, kMaxSuffix + 1);
    if (length > kMaxSuffix)
        return false;
    for (size_t i = 0; i < length; ++i) {
        if (text[i] < 0x20 || text[i] > 0x7e)
            return false;
    }
    out.assign(text, length);
    return true;
}

std::string text(const char* value) { return value ? value : ""; }

} // namespace

bool fullConfig(const webd_view_config& local, ViewConfig& vc, bool& deltas)
{
    vc.light = false;
    vc.configured = true;
    const int features = covers(local, offsetof(webd_view_config, features), sizeof local.features) ? local.features : 0;
    // Only what a view may have, and the policy always: every navigation of a full view is the service's to decide.
    vc.features = (features & kViewFeatures) | WEBD_FEATURE_POLICY;
    if (covers(local, offsetof(webd_view_config, profile), sizeof local.profile) && local.profile && *local.profile) {
        if (!cleanProfile(local.profile, vc.profile)) {
            say("FULL:REFUSED:profile is not a clean absolute directory");
            return false;
        }
    }
    vc.memoryOnly = covers(local, offsetof(webd_view_config, memory_only), sizeof local.memory_only) && local.memory_only == 1;
    // Anything but 0 is "no network": a field that is not exactly "off" fails closed.
    vc.noNetwork = covers(local, offsetof(webd_view_config, no_network), sizeof local.no_network) && local.no_network != 0;
    if (vc.noNetwork && (vc.features & WEBD_FEATURE_BLOCKING)) {
        say("FULL:REFUSED:no_network with content blocking (the lists' rebuilds would drop the lock)");
        return false;
    }
    // Absent in an older caller's struct: its own profile (0), as before the field existed.
    vc.storeKind = covers(local, offsetof(webd_view_config, store_kind), sizeof local.store_kind) ? local.store_kind : WEBD_STORE_OWN;
    deltas = covers(local, offsetof(webd_view_config, deltas), sizeof local.deltas) && local.deltas == 1;
    if (covers(local, offsetof(webd_view_config, user_agent_suffix), sizeof local.user_agent_suffix) && local.user_agent_suffix
        && *local.user_agent_suffix && !cleanSuffix(local.user_agent_suffix, vc.userAgentSuffix))
        say("CONFIG:IGNORED:user_agent_suffix");
    return true;
}

} // namespace webd
} // namespace webkitapp

using namespace webkitapp;
using namespace webkitapp::webd;

extern "C" {

int webd_engine_start_host(const webd_host_config* config)
{
    if (!claimStart())
        return 1;
    webd_host_config local = { };
    if (config)
        memcpy(&local, config, std::min<size_t>(config->struct_size, sizeof local));
    if (config && config->struct_size < offsetof(webd_host_config, children_directory) + sizeof local.children_directory)
        local.children_directory = nullptr;
    const std::string children = local.children_directory ? std::string(local.children_directory, strnlen(local.children_directory, kMaxPath)) : "";
    const bool started = children.empty() ? startHost(&env) : startHostWithChildren(&env, children);
    if (!started)
        return 1;
    // Light views too, when the service named their rule folder.
    prepareLightLock();
    return 0;
}

void webd_engine_load(uint64_t view, const char* url)
{
    withFullView(view, [address = text(url)](Slot&, View& v) { v.load(address); });
}

void webd_engine_reload(uint64_t view)
{
    withFullView(view, [](Slot&, View& v) { v.reload(); });
}

void webd_engine_stop(uint64_t view)
{
    withFullView(view, [](Slot&, View& v) { v.stop(); });
}

void webd_engine_back(uint64_t view)
{
    withFullView(view, [](Slot&, View& v) { v.back(); });
}

void webd_engine_forward(uint64_t view)
{
    withFullView(view, [](Slot&, View& v) { v.forward(); });
}

void webd_engine_download(uint64_t view, const char* url)
{
    withFullView(view, [address = text(url)](Slot&, View& v) {
        // Without downloads the "save" would be a plain load of the address: nothing at all instead.
        if (v.has(WEBD_FEATURE_DOWNLOADS))
            v.download(address);
    });
}

void webd_engine_download_cancel(uint64_t view, uint64_t download)
{
    withFullView(view, [download](Slot&, View& v) { v.downloadCancel(download); });
}

void webd_engine_dialog_reply(uint64_t view, uint64_t dialog, int accepted, const char* value)
{
    withFullView(view, [dialog, accepted, typed = text(value)](Slot&, View& v) { v.replyDialog(dialog, accepted != 0, typed); });
}

void webd_engine_navigation_answer(uint64_t view, uint64_t request, int allow)
{
    withFullView(view, [request, allow](Slot&, View& v) { v.answerNavigation(request, allow != 0); });
}

void webd_engine_notify_shown(uint64_t id)
{
    onMain([id] { notifyShown(id); });
}

void webd_engine_notify_clicked(uint64_t id)
{
    onMain([id] { notifyClicked(id); });
}

void webd_engine_notify_closed(uint64_t id)
{
    onMain([id] { notifyClosed(id); });
}

void webd_engine_notify_permission(uint64_t view, const char* origin, int answer)
{
    const int clamped = answer > 0 ? 1 : answer == 0 ? 0 : -1;
    withFullView(view, [site = text(origin), clamped](Slot&, View& v) { notifyPermissionIn(v, site, clamped); });
}

void webd_engine_blocking_load(uint64_t view, const char* list)
{
    withFullView(view, [name = text(list)](Slot&, View& v) { blockingLoad(v, name); });
}

void webd_engine_blocking_compile(uint64_t view, const char* list, const char* json, size_t len)
{
    std::string source(json ? json : "", json ? len : 0);
    withFullView(view, [name = text(list), source = std::move(source)](Slot&, View& v) { blockingCompile(v, name, source); });
}

void webd_engine_blocking_forget(uint64_t view, const char* list)
{
    withFullView(view, [name = text(list)](Slot&, View& v) { blockingForget(v, name); });
}

void webd_engine_recover(uint64_t view, int reload)
{
    withFullView(view, [reload](Slot&, View& v) { v.recover(reload != 0); });
}

void webd_engine_notify_forget(uint64_t view, const char* origin)
{
    // WebKit's record is the context's: the call is host-wide, the view only proves the caller has a full one.
    withFullView(view, [site = text(origin)](Slot&, View&) { notifyForget(site); });
}

} // extern "C"
