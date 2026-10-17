// Painting the page into a bitmap for the window (docs/webkit-port-plan.md, W5).
//
// One CPU runs the page's process, this one, the window and the compositor, so nothing here overlaps
// with anything else: every pass over the frame is paid for by whoever is scrolling
// (docs/lazywebplus-performance-plan.md, H1). The classic drawing area reports what changed, so only
// that rectangle is painted and converted, and paints are spaced so that the page's process gets the
// CPU back between them.
//
// Two bitmaps: WebKit paints into `paint.target`, BGRA as Skia makes it, and what the window gets
// (`E.pixels`) is opaque RGBA converted from it, rectangle by rectangle. A pixel WebKit does not
// write therefore stays what the last paint left, never converted twice.
//
// In an image built with the UI probe, `<profile>/paint-mode` (a file; absent means "fast:33") picks
// the path, for comparing them in one image: "legacy" paints and converts the whole view on every
// report, as this file used to, and "fast:<ms>" is the damage path with at least <ms> between the
// starts of two paints. (The profile folder is one the sandboxed NetworkProcess may write, so a
// shipped browser does not read it.)
//
// Once a second, while anything is painted (the last partial second of a burst is printed when the next
// event arrives, not before), the UI process prints
//   INFO:PAINT:mode=<m>:n=<paints>:needs=<reports>:px=<painted>:clear|wk|swz|post=<us, summed>:
//              avg|max=<us per paint>:lat=<longest us from a report to the paint that took it>:
//              wheel=<window calls>:mouse=<events>:w2d=<us, average/longest, wheel to first report>
// (the numbers the plan's P0 asks for; the window's own time is not in it).

#include <WebKit/WKGeometry.h>
#include <WebKit/WKPage.h>
#include <WebKit/WKPagePrivate.h>
#include <WebKit/WKPagePrivatePlayStation.h>
#include <WebKit/WKRunLoop.h>

#include <algorithm>
#include <math.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "engine.h"
#include "lazyos_ui.h"

namespace webkitapp {

namespace {

long long nowUs()
{
    timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec * 1000000LL + now.tv_nsec / 1000;
}

// ---- which path ------------------------------------------------------------------------

struct Mode {
    bool legacy { false };
    long long minIntervalUs { 33000 };
};

const Mode& mode()
{
    static const Mode chosen = [] {
        Mode m;
        const char* profile = getenv("LAZYOS_WEBKIT_PROFILE");
        if (!harness() || !profile || !*profile)
            return m;
        std::string path(profile);
        path += "/paint-mode";
        FILE* file = fopen(path.c_str(), "r");
        if (!file)
            return m;
        char text[32] = { 0 };
        if (fgets(text, sizeof(text), file)) {
            if (!strncmp(text, "legacy", 6)) {
                m.legacy = true;
                m.minIntervalUs = 0;
            } else if (!strncmp(text, "fast:", 5)) {
                m.minIntervalUs = std::max(0L, std::min(1000L, atol(text + 5))) * 1000LL;
            }
        }
        fclose(file);
        return m;
    }();
    return chosen;
}

// ---- damage ----------------------------------------------------------------------------

struct Damage {
    int x0 { 0 }, y0 { 0 }, x1 { 0 }, y1 { 0 };
    bool empty() const { return x1 <= x0 || y1 <= y0; }
    void add(int ax0, int ay0, int ax1, int ay1)
    {
        if (ax1 <= ax0 || ay1 <= ay0)
            return;
        if (empty()) {
            x0 = ax0, y0 = ay0, x1 = ax1, y1 = ay1;
            return;
        }
        x0 = std::min(x0, ax0), y0 = std::min(y0, ay0), x1 = std::max(x1, ax1), y1 = std::max(y1, ay1);
    }
};

struct Painting {
    Damage damage;
    // Everything must be painted: the first frame, a new size, a caller that names no rectangle.
    bool full { true };
    long long lastStartUs { 0 };
    // When the first report since the last paint arrived (0: none is waiting).
    long long pendingSinceUs { 0 };
    // The size of both bitmaps: compared by width and height, never by byte count (1000x450 and
    // 900x500 are the same number of bytes).
    int width { 0 }, height { 0 };
    // What WebKit paints into: BGRA, premultiplied, as Skia's N32 is on this target.
    std::vector<uint8_t> target;
};
Painting paint;

// ---- counters --------------------------------------------------------------------------

struct Stats {
    long long windowStartUs { 0 };
    long paints { 0 }, needs { 0 }, wheels { 0 }, mice { 0 };
    long long pixels { 0 };
    long long clearUs { 0 }, paintUs { 0 }, swizzleUs { 0 }, postUs { 0 }, maxUs { 0 }, totalUs { 0 };
    long long maxLatencyUs { 0 };
    long long w2dSumUs { 0 }, w2dMaxUs { 0 };
    long w2dCount { 0 };
};
Stats stats;
// When the wheel was last turned and no report has answered it yet (0: none); a turn that moved
// nothing is forgotten after half a second, not charged to whatever damage comes next.
long long wheelTurnedUs { 0 };
constexpr long long kWheelAnswerUs = 500000;

void report(long long now)
{
    if (!stats.windowStartUs)
        stats.windowStartUs = now;
    if (now - stats.windowStartUs < 1000000)
        return;
    if (stats.paints || stats.needs || stats.wheels || stats.mice) {
        say("INFO:PAINT:mode=%s:n=%ld:needs=%ld:px=%lld:clear=%lld:wk=%lld:swz=%lld:post=%lld:avg=%lld:max=%lld:lat=%lld:"
            "wheel=%ld:mouse=%ld:w2d=%lld/%lld",
            mode().legacy ? "legacy" : "fast", stats.paints, stats.needs, stats.pixels, stats.clearUs, stats.paintUs,
            stats.swizzleUs, stats.postUs, stats.paints ? stats.totalUs / stats.paints : 0LL, stats.maxUs,
            stats.maxLatencyUs, stats.wheels, stats.mice, stats.w2dCount ? stats.w2dSumUs / stats.w2dCount : 0LL,
            stats.w2dMaxUs);
    }
    stats = Stats();
    stats.windowStartUs = now;
}

// ---- conversion ------------------------------------------------------------------------

// Skia's BGRA to the window's opaque RGBA, one 32-bit load and store per pixel (a byte at a time
// was most of the cost of a repaint). `source` and `destination` have the same stride.
void convertRect(const uint8_t* source, uint8_t* destination, int stride, const Damage& r)
{
    for (int y = r.y0; y < r.y1; ++y) {
        const uint8_t* in = source + static_cast<size_t>(y) * stride + static_cast<size_t>(r.x0) * 4;
        uint8_t* out = destination + static_cast<size_t>(y) * stride + static_cast<size_t>(r.x0) * 4;
        for (int x = 0, n = r.x1 - r.x0; x < n; ++x) {
            uint32_t v;
            memcpy(&v, in + 4 * x, 4);
            v = 0xff000000u | ((v & 0xffu) << 16) | (v & 0x0000ff00u) | ((v >> 16) & 0xffu);
            memcpy(out + 4 * x, &v, 4);
        }
    }
}

// The legacy path's conversion: every byte of the frame, one at a time.
void convertBytes(const uint8_t* source, uint8_t* destination, size_t bytes)
{
    for (size_t i = 0; i < bytes; i += 4) {
        destination[i] = source[i + 2];
        destination[i + 1] = source[i + 1];
        destination[i + 2] = source[i];
        destination[i + 3] = 0xff;
    }
}

// ---- the paint -------------------------------------------------------------------------

void paintNow(void*)
{
    E.paintScheduled = false;
    if (!E.page || E.width <= 0 || E.height <= 0)
        return;
    const bool legacy = mode().legacy;
    const long long t0 = nowUs();
    const size_t bytes = static_cast<size_t>(E.width) * E.height * 4;
    bool full = paint.full || legacy;
    if (paint.width != E.width || paint.height != E.height || paint.target.size() != bytes) {
        // A new size: both bitmaps start white (what the page does not cover is white, never
        // see-through), once and not on every paint.
        paint.target.assign(bytes, 0xff);
        E.pixels.assign(bytes, 0xff);
        paint.width = E.width;
        paint.height = E.height;
        full = true;
    }
    Damage rect = paint.damage;
    if (full)
        rect = Damage { 0, 0, E.width, E.height };
    rect.x0 = std::max(rect.x0, 0), rect.y0 = std::max(rect.y0, 0);
    rect.x1 = std::min(rect.x1, E.width), rect.y1 = std::min(rect.y1, E.height);
    paint.damage = Damage();
    paint.full = false;
    const long long pendingSince = paint.pendingSinceUs;
    paint.pendingSinceUs = 0;
    if (rect.empty())
        return;
    paint.lastStartUs = t0;

    if (legacy)
        memset(paint.target.data(), 0xff, bytes);
    const long long t1 = nowUs();
    WKPagePaint(E.page, paint.target.data(), WKSizeMake(E.width, E.height),
        WKRectMake(rect.x0, rect.y0, rect.x1 - rect.x0, rect.y1 - rect.y0));
    const long long t2 = nowUs();
    if (legacy)
        convertBytes(paint.target.data(), E.pixels.data(), bytes);
    else
        convertRect(paint.target.data(), E.pixels.data(), E.width * 4, rect);
    const long long t3 = nowUs();
    lzw_ui_frame(E.pixels.data(), E.width, E.height);
    const long long t4 = nowUs();

    ++stats.paints;
    stats.pixels += static_cast<long long>(rect.x1 - rect.x0) * (rect.y1 - rect.y0);
    stats.clearUs += t1 - t0, stats.paintUs += t2 - t1, stats.swizzleUs += t3 - t2, stats.postUs += t4 - t3;
    stats.totalUs += t4 - t0;
    stats.maxUs = std::max(stats.maxUs, t4 - t0);
    if (pendingSince)
        stats.maxLatencyUs = std::max(stats.maxLatencyUs, t0 - pendingSince);
    report(t4);

    if (E.firstPaintMs < 0 && E.committedSincePaint) {
        E.firstPaintMs = elapsedMs();
        say("INFO:TIME:firstpaint=%ld:sincecommit=%ld:sincenav=%ld", E.firstPaintMs, E.firstPaintMs - E.commitMs,
            E.firstPaintMs - E.navStartMs);
    }
}

// ---- pacing ----------------------------------------------------------------------------

// WKRunLoop has no timer, so a thread sleeps the gap and posts the paint; there is one, started
// the first time a paint has to wait.
struct Pacer {
    pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;
    pthread_cond_t cond = PTHREAD_COND_INITIALIZER;
    long long delayUs { -1 };
    bool started { false };
};
Pacer pacer;

void* pacerMain(void*)
{
    for (;;) {
        pthread_mutex_lock(&pacer.mutex);
        while (pacer.delayUs < 0)
            pthread_cond_wait(&pacer.cond, &pacer.mutex);
        const long long delay = pacer.delayUs;
        pacer.delayUs = -1;
        pthread_mutex_unlock(&pacer.mutex);
        if (delay > 0)
            usleep(static_cast<useconds_t>(delay));
        WKRunLoopCallOnMainThread(paintNow, nullptr);
    }
    return nullptr;
}

void paintAfter(long long delayUs)
{
    if (!pacer.started) {
        pthread_t thread;
        if (pthread_create(&thread, nullptr, pacerMain, nullptr) != 0) {
            WKRunLoopCallOnMainThread(paintNow, nullptr);
            return;
        }
        pthread_detach(thread);
        pacer.started = true;
    }
    pthread_mutex_lock(&pacer.mutex);
    pacer.delayUs = delayUs;
    pthread_cond_signal(&pacer.cond);
    pthread_mutex_unlock(&pacer.mutex);
}

// A paint will run, now or after the gap; one at a time, however many reports arrive meanwhile.
void requestPaint(long long now)
{
    if (!paint.pendingSinceUs)
        paint.pendingSinceUs = now;
    if (E.paintScheduled)
        return;
    E.paintScheduled = true;
    const long long wait = paint.lastStartUs + mode().minIntervalUs - now;
    if (wait <= 0)
        WKRunLoopCallOnMainThread(paintNow, nullptr);
    else
        paintAfter(wait);
}

} // namespace

// A caller that names no rectangle (a load finished, the window changed size): all of it.
void schedulePaint()
{
    paint.full = true;
    requestPaint(nowUs());
}

void viewNeedsDisplay(WKViewRef, WKRect rect, const void*)
{
    // Many rectangles arrive for one change: the union is painted once.
    paint.damage.add(static_cast<int>(floor(rect.origin.x)), static_cast<int>(floor(rect.origin.y)),
        static_cast<int>(ceil(rect.origin.x + rect.size.width)), static_cast<int>(ceil(rect.origin.y + rect.size.height)));
    const long long now = nowUs();
    report(now); // a window that ended is printed before this event is counted
    ++stats.needs;
    if (wheelTurnedUs) {
        const long long latency = now - wheelTurnedUs;
        wheelTurnedUs = 0;
        if (latency <= kWheelAnswerUs) {
            stats.w2dSumUs += latency, ++stats.w2dCount;
            stats.w2dMaxUs = std::max(stats.w2dMaxUs, latency);
        }
    }
    requestPaint(now);
}

void notePointer(bool wheel)
{
    const long long now = nowUs();
    report(now);
    if (!wheel) {
        ++stats.mice;
        return;
    }
    ++stats.wheels;
    if (!wheelTurnedUs || now - wheelTurnedUs > kWheelAnswerUs)
        wheelTurnedUs = now;
}

// The page's process is gone: what both bitmaps hold is a page that no longer exists, and the next
// paint, whenever the new process answers, starts from white.
void invalidatePaint()
{
    paint.width = 0;
    paint.height = 0;
    paint.full = true;
}

} // namespace webkitapp
