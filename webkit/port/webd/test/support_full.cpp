// The webd_ui_* and webd_paint_* callbacks of full views, as the service half would answer them, and the
// helpers the full modes share (support.h). The damage a delta view's paints need is kept here per view, as
// xui-app/crates/damage does for the browser, in its simplest correct form: one scroll and a few rectangles,
// anything else is a whole frame.

#include "support.h"

#include <algorithm>
#include <ctype.h>
#include <filesystem>
#include <stdio.h>
#include <string.h>

namespace webdtest {

std::string childrenDirectory;

namespace {

using detail::changed;
using detail::lock;
using detail::record;
using detail::told;

Answers answers;

constexpr int kMaxRects = 8;

// What changed in a delta view since its last paint.
struct Damage {
    bool full { true };
    bool scrolled { false };
    int scroll[6] = { 0, 0, 0, 0, 0, 0 };
    std::vector<int> rects;
};
std::map<uint64_t, Damage> damage;

void addRects(Damage& d, const int* rects, int count)
{
    for (int i = 0; i < 4 * count; ++i)
        d.rects.push_back(rects[i]);
    if (d.rects.size() > static_cast<size_t>(4 * kMaxRects))
        d.full = true;
}

// Moves the area of the picture by (dx, dy) as the service moves it: what was inside goes on, what it uncovers
// keeps its old pixels (paint.cpp's shadowScroll).
void scrollPicture(ViewFacts& f, const int* s)
{
    const int x0 = std::max(s[0], 0), y0 = std::max(s[1], 0), x1 = std::min(s[0] + s[2], f.pictureWidth), y1 = std::min(s[1] + s[3], f.pictureHeight);
    const int dx = s[4], dy = s[5];
    const int tx0 = std::max(x0 + dx, x0), ty0 = std::max(y0 + dy, y0), tx1 = std::min(x1 + dx, x1), ty1 = std::min(y1 + dy, y1);
    if (tx1 <= tx0 || ty1 <= ty0)
        return;
    const size_t stride = static_cast<size_t>(f.pictureWidth) * 4;
    const size_t bytes = static_cast<size_t>(tx1 - tx0) * 4;
    for (int i = 0; i < ty1 - ty0; ++i) {
        const int row = dy > 0 ? ty1 - ty0 - 1 - i : i;
        memmove(f.picture.data() + static_cast<size_t>(ty0 + row) * stride + static_cast<size_t>(tx0) * 4,
            f.picture.data() + static_cast<size_t>(ty0 + row - dy) * stride + static_cast<size_t>(tx0 - dx) * 4, bytes);
    }
}

} // namespace

uint64_t createFull(const FullOptions& options)
{
    const uint64_t id = detail::newId();
    webd_view_config config = { };
    config.struct_size = sizeof config;
    config.light = 0;
    config.scripts = options.scripts;
    config.width = options.width;
    config.height = options.height;
    config.group = options.group;
    config.features = options.features;
    config.profile = options.profile.c_str();
    config.memory_only = options.memoryOnly;
    config.deltas = options.deltas;
    config.user_agent_suffix = options.userAgentSuffix.empty() ? nullptr : options.userAgentSuffix.c_str();
    config.no_network = options.noNetwork;
    config.store_kind = options.storeKind;
    webd_engine_view_create(id, &config);
    return id;
}

void setAnswers(const Answers& given)
{
    std::lock_guard<std::mutex> hold(lock());
    answers = given;
}

std::string describeFull(const ViewFacts& f)
{
    char text[700];
    snprintf(text, sizeof text, "url='%s' committed='%s' title='%s' finished=%d loading=%d back=%d forward=%d error='%s' frames=%d deltas=%d navigations=%zu lost=%d",
        f.url.substr(0, 120).c_str(), f.committed.substr(0, 120).c_str(), f.title.substr(0, 80).c_str(), f.finished, f.loading, f.canBack,
        f.canForward, f.error.substr(0, 120).c_str(), f.frames, f.deltas, f.navigations.size(), f.lostReason);
    return text;
}

void click(uint64_t view, int x, int y)
{
    webd_engine_mouse(view, 0, 0, x, y, 0);
    webd_engine_mouse(view, 1, 0, x, y, 0);
    webd_engine_mouse(view, 2, 0, x, y, 0);
}

bool waitPage(uint64_t view, const std::string& title, int ms)
{
    return waitView(view, [&](const ViewFacts& f) { return f.title == title && !f.loading && f.finished >= 1; }, ms);
}

std::string linkPage(const std::string& title, const std::string& href, const std::string& more)
{
    std::string body;
    if (!href.empty())
        body = "<a href=\"" + href + "\" style=\"display:block;position:absolute;left:0;top:0;width:200px;height:100px;background:#f00\">go</a>";
    return "<!doctype html><title>" + title + "</title><body style=\"margin:0;background:#fff\">" + body + more;
}

std::string scratchFolder(const char* name)
{
    std::string path = std::filesystem::temp_directory_path().string() + "/webd-abi-" + name + "-XXXXXX";
    std::vector<char> buffer(path.begin(), path.end());
    buffer.push_back(0);
    return mkdtemp(buffer.data()) ? std::string(buffer.data()) : std::string();
}

} // namespace webdtest

using namespace webdtest;

extern "C" {

void webd_ui_page_state(uint64_t view, const webd_page_state* s)
{
    std::lock_guard<std::mutex> hold(lock());
    ViewFacts& f = record(view);
    told(f);
    f.url = s->url;
    f.title = s->title;
    f.loading = s->loading;
    f.canBack = s->can_back;
    f.canForward = s->can_forward;
    f.progress = s->progress;
    f.error = s->error;
    f.committed = s->committed_url;
    f.secure = s->secure;
    f.finished = s->finished;
    ++f.pageStates;
    ++f.states;
    changed().notify_all();
}

void webd_paint_resize(uint64_t view, int, int)
{
    std::lock_guard<std::mutex> hold(lock());
    damage[view] = Damage();
}

void webd_paint_full(uint64_t view)
{
    std::lock_guard<std::mutex> hold(lock());
    damage[view].full = true;
}

void webd_paint_rect(uint64_t view, int x, int y, int width, int height)
{
    const int rect[4] = { x, y, width, height };
    std::lock_guard<std::mutex> hold(lock());
    addRects(damage[view], rect, 1);
}

void webd_paint_update(uint64_t view, int sx, int sy, int sw, int sh, int dx, int dy, const int* rects, int count)
{
    std::lock_guard<std::mutex> hold(lock());
    Damage& d = damage[view];
    if (sw > 0 && sh > 0 && (dx || dy)) {
        // A second scroll, or rectangles from before it (they would have to move with it): a whole frame.
        if (d.scrolled || !d.rects.empty())
            d.full = true;
        d.scrolled = true;
        const int s[6] = { sx, sy, sw, sh, dx, dy };
        memcpy(d.scroll, s, sizeof s);
    }
    addRects(d, rects, count);
}

int webd_paint_plan(uint64_t view, int* scroll, int* rects, int max, int* count)
{
    std::lock_guard<std::mutex> hold(lock());
    Damage& d = damage[view];
    Damage taken = d;
    d = Damage();
    d.full = false;
    *count = 0;
    if (taken.full || taken.rects.size() > static_cast<size_t>(4 * max))
        return WEBD_PAINT_FULL;
    if (!taken.scrolled && taken.rects.empty())
        return WEBD_PAINT_NOTHING;
    memcpy(scroll, taken.scroll, sizeof taken.scroll);
    std::copy(taken.rects.begin(), taken.rects.end(), rects);
    *count = static_cast<int>(taken.rects.size() / 4);
    return WEBD_PAINT_DELTA;
}

int webd_ui_frame_delta(uint64_t view, const uint8_t* rgba, int width, int height, const int* scroll, const int* rects, int count)
{
    std::lock_guard<std::mutex> hold(lock());
    ViewFacts& f = record(view);
    told(f);
    if (!f.scrollAnnounced)
        ++f.unannounced;
    f.scrollAnnounced = false;
    if (f.pictureWidth != width || f.pictureHeight != height || f.picture.empty())
        return 1; // nothing to lay it over: the next one is a whole frame
    if (scroll) {
        scrollPicture(f, scroll);
        ++f.movedDeltas;
    }
    const size_t stride = static_cast<size_t>(width) * 4;
    for (int i = 0; i < count; ++i) {
        const int x = rects[4 * i], y = rects[4 * i + 1], w = rects[4 * i + 2], h = rects[4 * i + 3];
        for (int row = y; row < y + h; ++row)
            memcpy(f.picture.data() + row * stride + static_cast<size_t>(x) * 4, rgba + row * stride + static_cast<size_t>(x) * 4, static_cast<size_t>(w) * 4);
    }
    ++f.deltas;
    changed().notify_all();
    return 0;
}

void webd_ui_band(uint64_t view, int, int, int, int, const uint8_t*)
{
    std::lock_guard<std::mutex> hold(lock());
    ViewFacts& f = record(view);
    told(f);
    ++f.bands;
    changed().notify_all();
}

void webd_ui_hover(uint64_t view, const char*)
{
    std::lock_guard<std::mutex> hold(lock());
    told(record(view));
}

void webd_ui_cursor(uint64_t view, int)
{
    std::lock_guard<std::mutex> hold(lock());
    told(record(view));
}

void webd_ui_new_window(uint64_t view, const char* url, int gesture)
{
    std::lock_guard<std::mutex> hold(lock());
    ViewFacts& f = record(view);
    told(f);
    f.newWindows.emplace_back(url, gesture);
    changed().notify_all();
}

void webd_ui_responsive(uint64_t view, int)
{
    std::lock_guard<std::mutex> hold(lock());
    told(record(view));
}

void webd_ui_dialog(uint64_t view, uint64_t dialog, int kind, const char* message, const char* defaultValue)
{
    std::lock_guard<std::mutex> hold(lock());
    ViewFacts& f = record(view);
    told(f);
    f.dialogs.push_back(Dialog { dialog, kind, message, defaultValue });
    changed().notify_all();
}

int webd_ui_download_start(uint64_t view, uint64_t download, const char* url, const char* name, int64_t total, char* path, size_t size)
{
    std::lock_guard<std::mutex> hold(lock());
    ViewFacts& f = record(view);
    told(f);
    Download& d = f.downloads[download];
    d.url = url;
    d.name = name;
    d.total = total;
    changed().notify_all();
    if (answers.downloadDir.empty())
        return 1;
    // The name is the page's: only its letters reach the path (the service's sanitiser in miniature).
    std::string leaf;
    for (char c : d.name)
        leaf += isalnum(static_cast<unsigned char>(c)) || c == '.' ? c : '_';
    d.path = answers.downloadDir + "/" + std::to_string(download) + "-" + leaf + ".part";
    if (d.path.size() + 1 > size)
        return 1;
    memcpy(path, d.path.c_str(), d.path.size() + 1);
    return 0;
}

void webd_ui_download_progress(uint64_t view, uint64_t download, int64_t received, int64_t total)
{
    std::lock_guard<std::mutex> hold(lock());
    ViewFacts& f = record(view);
    told(f);
    Download& d = f.downloads[download];
    d.received = received;
    d.total = total;
    ++d.progress;
    changed().notify_all();
}

void webd_ui_download_done(uint64_t view, uint64_t download)
{
    std::lock_guard<std::mutex> hold(lock());
    ViewFacts& f = record(view);
    told(f);
    f.downloads[download].done = true;
    changed().notify_all();
}

void webd_ui_download_fail(uint64_t view, uint64_t download, const char* reason)
{
    std::lock_guard<std::mutex> hold(lock());
    ViewFacts& f = record(view);
    told(f);
    f.downloads[download].failed = true;
    f.downloads[download].reason = reason;
    changed().notify_all();
}

int webd_ui_navigate(uint64_t view, uint64_t request, const char* url, int kind, int gesture, int mainFrame, const char* committed)
{
    Navigation navigation { request, url, committed, kind, gesture, mainFrame, WEBD_VERDICT_ALLOW };
    std::function<int(uint64_t, const Navigation&)> decide;
    {
        std::lock_guard<std::mutex> hold(lock());
        decide = answers.navigate;
    }
    // The mode's answer, outside the lock (it may read facts).
    if (decide)
        navigation.verdict = decide(view, navigation);
    std::lock_guard<std::mutex> hold(lock());
    ViewFacts& f = record(view);
    told(f);
    f.navigations.push_back(navigation);
    changed().notify_all();
    return navigation.verdict;
}

void webd_ui_notify_show(uint64_t view, uint64_t id, const char* origin, const char*, const char* title, const char* body, const char*)
{
    std::lock_guard<std::mutex> hold(lock());
    ViewFacts& f = record(view);
    told(f);
    f.notifications.push_back({ origin, title, body });
    f.notificationIds.push_back(id);
    changed().notify_all();
}

void webd_ui_notify_cancel(uint64_t view, uint64_t)
{
    std::lock_guard<std::mutex> hold(lock());
    told(record(view));
}

void webd_ui_notify_gone(uint64_t view, uint64_t)
{
    std::lock_guard<std::mutex> hold(lock());
    told(record(view));
}

void webd_ui_notify_permission_request(uint64_t view, const char* origin)
{
    std::lock_guard<std::mutex> hold(lock());
    ViewFacts& f = record(view);
    told(f);
    f.permissionRequests.push_back(origin);
    changed().notify_all();
}

void webd_ui_notify_permission_gone(uint64_t view, const char*)
{
    std::lock_guard<std::mutex> hold(lock());
    told(record(view));
}

int webd_ui_blocking(uint64_t view, const char* url)
{
    std::function<int(uint64_t, const std::string&)> decide;
    {
        std::lock_guard<std::mutex> hold(lock());
        told(record(view));
        decide = answers.blocking;
    }
    return decide ? decide(view, url) : 1;
}

void webd_ui_blocking_list(uint64_t view, const char* list, int status)
{
    std::lock_guard<std::mutex> hold(lock());
    ViewFacts& f = record(view);
    told(f);
    f.blockingLists.emplace_back(list, status);
    changed().notify_all();
}

void webd_ui_blocked(uint64_t view, int count)
{
    std::lock_guard<std::mutex> hold(lock());
    ViewFacts& f = record(view);
    told(f);
    f.blocked = count;
    ++f.blockedReports;
    changed().notify_all();
}

} // extern "C"
