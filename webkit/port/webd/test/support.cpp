// The webd_ui_* callbacks of the test, and the helpers the modes share (support.h).

#include "support.h"

#include <algorithm>
#include <chrono>
#include <condition_variable>
#include <map>
#include <mutex>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

namespace webdtest {

namespace {

std::mutex mutex;
std::condition_variable changed;
std::map<uint64_t, ViewFacts> table;
Processes counted;
std::vector<std::string> lines;
uint64_t nextId = 1;

// The record of an id; the lock is held.
ViewFacts& record(uint64_t id) { return table[id]; }

// Anything the engine says about an id after its promise of silence.
void told(ViewFacts& f)
{
    if (f.silent)
        ++f.violations;
}

} // namespace

namespace detail {
std::mutex& lock() { return mutex; }
std::condition_variable& changed() { return webdtest::changed; }
ViewFacts& record(uint64_t id) { return webdtest::record(id); }
void told(ViewFacts& f) { webdtest::told(f); }
uint64_t newId()
{
    std::lock_guard<std::mutex> hold(mutex);
    const uint64_t id = nextId++;
    record(id);
    return id;
}
} // namespace detail

namespace {

void fenceTask(void* context)
{
    std::lock_guard<std::mutex> hold(mutex);
    record(reinterpret_cast<uintptr_t>(context)).silent = true;
    changed.notify_all();
}

} // namespace

ViewFacts facts(uint64_t id)
{
    std::lock_guard<std::mutex> hold(mutex);
    return record(id);
}

bool waitFor(const std::function<bool()>& ready, int ms)
{
    std::unique_lock<std::mutex> hold(mutex);
    return changed.wait_for(hold, std::chrono::milliseconds(ms), ready);
}

bool waitView(uint64_t id, const std::function<bool(const ViewFacts&)>& ready, int ms)
{
    return waitFor([&] { return ready(record(id)); }, ms);
}

Processes processes()
{
    std::lock_guard<std::mutex> hold(mutex);
    return counted;
}

bool waitProcesses(const std::function<bool(const Processes&)>& ready, int ms)
{
    return waitFor([&] { return ready(counted); }, ms);
}

bool said(const std::string& prefix)
{
    std::lock_guard<std::mutex> hold(mutex);
    return std::any_of(lines.begin(), lines.end(), [&](const std::string& l) { return l.compare(0, prefix.size(), prefix) == 0; });
}

PoolCounts poolCounts()
{
    std::lock_guard<std::mutex> hold(mutex);
    PoolCounts counts;
    for (auto it = lines.rbegin(); it != lines.rend(); ++it) {
        // "INFO:POOL:reused=%llu:pooled=%zu:live=%zu:retired=%zu"
        if (it->compare(0, 10, "INFO:POOL:") != 0)
            continue;
        sscanf(it->c_str(), "INFO:POOL:reused=%ld:pooled=%ld:live=%ld:retired=%ld",
            &counts.reused, &counts.pooled, &counts.live, &counts.retired);
        break;
    }
    return counts;
}

uint64_t create(const Options& options)
{
    const uint64_t id = detail::newId();
    webd_view_config config = { };
    config.struct_size = sizeof config;
    config.light = 1;
    config.scripts = options.scripts;
    config.width = options.width;
    config.height = options.height;
    config.group = options.group;
    config.color_scheme = options.colorScheme;
    config.default_font_size = options.fontSize;
    config.user_style = options.style;
    webd_engine_view_create(id, &config);
    return id;
}

void loadHtml(uint64_t id, const std::string& html, const char* base)
{
    webd_engine_load_html(id, reinterpret_cast<const uint8_t*>(html.data()), html.size(), base);
}

void destroy(uint64_t id)
{
    webd_engine_view_destroy(id);
    webd_engine_run_on_main(fenceTask, reinterpret_cast<void*>(static_cast<uintptr_t>(id)));
    waitView(id, [](const ViewFacts& f) { return f.silent; }, 30000);
}

std::string page(const char* title, const char* colour, const std::string& body)
{
    return std::string("<!doctype html><title>") + title + "</title><body style=\"margin:0;background:" + colour + "\">" + body;
}

bool shows(const ViewFacts& f, const char* title, char colour, int finished)
{
    return f.finished >= finished && f.title == title && f.last == colour;
}

char dominant(const uint8_t* rgba, int width, int height)
{
    long counts[4] = { 0, 0, 0, 0 };
    const long total = static_cast<long>(width) * height;
    for (long i = 0; i < total; ++i) {
        const uint8_t r = rgba[4 * i], g = rgba[4 * i + 1], b = rgba[4 * i + 2];
        if (r > 200 && g < 80 && b < 80)
            ++counts[0];
        else if (g > 100 && r < 80 && b < 80)
            ++counts[1];
        else if (b > 200 && r < 80 && g < 80)
            ++counts[2];
        else if (r > 240 && g > 240 && b > 240)
            ++counts[3];
    }
    const char names[4] = { 'r', 'g', 'b', 'w' };
    for (int i = 0; i < 4; ++i) {
        if (counts[i] * 2 > total)
            return names[i];
    }
    return '?';
}

long monotonicMs()
{
    timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec * 1000L + now.tv_nsec / 1000000L;
}

void note(const char* format, ...)
{
    char line[512];
    va_list args;
    va_start(args, format);
    vsnprintf(line, sizeof line, format, args);
    va_end(args);
    printf("WEBD:%s\n", line);
    fflush(stdout);
}

} // namespace webdtest

using namespace webdtest;

extern "C" {

void webd_ui_state(uint64_t view, const char* title, int loading, int finished, const char* error)
{
    std::lock_guard<std::mutex> hold(mutex);
    ViewFacts& f = record(view);
    told(f);
    f.title = title ? title : "";
    f.error = error ? error : "";
    f.loading = loading;
    f.finished = finished;
    ++f.states;
    changed.notify_all();
}

void webd_ui_frame(uint64_t view, const uint8_t* rgba, int width, int height)
{
    const char colour = dominant(rgba, width, height);
    std::lock_guard<std::mutex> hold(mutex);
    ViewFacts& f = record(view);
    told(f);
    ++f.frames;
    f.width = width;
    f.height = height;
    f.last = colour;
    f.seen += colour;
    // The picture a delta view's posts are laid over (support_full.cpp), and whether a scroll came first.
    f.picture.assign(rgba, rgba + static_cast<size_t>(width) * height * 4);
    f.pictureWidth = width;
    f.pictureHeight = height;
    if (!f.scrollAnnounced)
        ++f.unannounced;
    f.scrollAnnounced = false;
    changed.notify_all();
}

void webd_ui_scroll(uint64_t view, int x, int y, int)
{
    std::lock_guard<std::mutex> hold(mutex);
    ViewFacts& f = record(view);
    told(f);
    ++f.scrolls;
    f.scrollAnnounced = true;
    f.scrollX = x;
    f.scrollY = y;
    changed.notify_all();
}

void webd_ui_link_clicked(uint64_t view, const char* url)
{
    std::lock_guard<std::mutex> hold(mutex);
    ViewFacts& f = record(view);
    told(f);
    f.links.push_back(url ? url : "");
    changed.notify_all();
}

void webd_ui_clipboard_write(uint64_t view, const char* text)
{
    std::lock_guard<std::mutex> hold(mutex);
    ViewFacts& f = record(view);
    told(f);
    f.clipboard.push_back(text ? text : "");
    changed.notify_all();
}

void webd_ui_view_lost(uint64_t view, int reason)
{
    std::lock_guard<std::mutex> hold(mutex);
    ViewFacts& f = record(view);
    told(f);
    f.lost = true;
    f.lostReason = reason;
    // The id is dead: the engine tells nothing more about it.
    f.silent = true;
    changed.notify_all();
}

void webd_ui_say(const char* line)
{
    const std::string text = line ? line : "";
    {
        std::lock_guard<std::mutex> hold(mutex);
        lines.push_back(text);
        const bool failed = text.find("FAIL") != std::string::npos;
        if (text.compare(0, 19, "PROCESS:LAUNCH:web:") == 0 && !failed)
            ++counted.webLaunches;
        else if (text.compare(0, 23, "PROCESS:LAUNCH:network:") == 0 && !failed)
            ++counted.networkLaunches;
        else if (text.compare(0, 17, "PROCESS:EXIT:web:") == 0)
            ++counted.webExits;
        else if (text.compare(0, 21, "PROCESS:EXIT:network:") == 0)
            ++counted.networkExits;
        changed.notify_all();
    }
    printf("WEBD:%s\n", text.c_str());
    fflush(stdout);
}

int webd_ui_user_memory(uint64_t*, uint64_t*) { return 1; }

// The test keeps no decisions a page process would start with (its notify mode answers each request).
int webd_ui_notify_permission_at(size_t, char*, size_t) { return -1; }

} // extern "C"
