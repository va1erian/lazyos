// Painting the page into a bitmap for the window (docs/webkit-port-plan.md, W5).
//
// One CPU runs the page's process, this one, the window and the compositor, so nothing here overlaps
// with anything else: every pass over the frame is paid for by whoever is scrolling
// (docs/lazywebplus-performance-plan.md, H1). The classic drawing area reports what changed, so only
// that is painted, converted and handed on, and paints are spaced so that the page's process gets the
// CPU back between them.
//
// What changed comes from the page's updates (WKPageSetUpdateCallback, patch 0024): the area that
// scrolled, by how much, and the rectangles repainted. WebKit moved its backing store itself and the
// web process painted only the rows the scroll exposed; this file does not copy the rest again. The
// window's library keeps the updates between two paints in the form a paint needs (damage.rs: one
// scroll and a few rectangles, or "all of it"), and the paint hands it the same: the window moves
// the picture it has and lays the rectangles over it (lzw_ui_frame_delta). A load, a resize, a page
// process that came back and anything the rules cannot tell as a scroll and rectangles is a whole
// frame (lzw_ui_frame), as every paint used to be.
//
// Two bitmaps: WebKit paints into `paint.target`, BGRA as Skia makes it, and what the window gets
// (`E.pixels`) is opaque RGBA converted from it, rectangle by rectangle. Only the rectangles of a
// delta are read from `E.pixels`, so it need not scroll with the picture.
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
//              wheel=<window calls>:mouse=<events>:w2d=<us, average/longest, wheel to first report>:
//              full=<whole frames>:delta=<deltas>:scroll=<deltas that moved the picture>:
//              checks=<comparisons with a whole paint>:bad=<those that differed> (probe images):
//              wkcalls=<WKPagePaint calls>:wkmax=<the longest, us>
// and, next to it, INFO:WKPAINT:calls|total|wrap|endprint|area|snapshot|draw|whole=<us, summed over the second>:
// what a WKPagePaint costs inside the page's process (patches 0025 and 0026). **The `wk` above is wall
// time around the call and includes whatever else the one CPU ran meanwhile (the window, the compositor):
// the call itself was measured at about 60 us. Read CPU per frame from PERF:CPU, not from the stages.**
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
    // Probe images keep the picture the window should have and compare it with a whole paint now
    // and then (see "the check" below).
    bool verify { false };
};

const Mode& mode()
{
    static const Mode chosen = [] {
        Mode m;
        m.verify = harness();
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

// A rectangle of the view, x0..x1 by y0..y1.
struct Damage {
    int x0 { 0 }, y0 { 0 }, x1 { 0 }, y1 { 0 };
    bool empty() const { return x1 <= x0 || y1 <= y0; }
};

// What changed is kept by the window's library (lzw_paint_*); this is only what a paint needs of its own.
struct Painting {
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
    long fulls { 0 }, deltas { 0 }, scrolls { 0 }, checks { 0 }, bad { 0 }, wkCalls { 0 };
    long long wkMaxUs { 0 };
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

// WKPagePaint's own time, as the page's process counts it (patch 0025), at the last report.
unsigned long long lastWk[8];

void report(long long now)
{
    if (!stats.windowStartUs)
        stats.windowStartUs = now;
    if (now - stats.windowStartUs < 1000000)
        return;
    if (stats.paints || stats.needs || stats.wheels) {
        say("INFO:PAINT:mode=%s:n=%ld:needs=%ld:px=%lld:clear=%lld:wk=%lld:swz=%lld:post=%lld:avg=%lld:max=%lld:lat=%lld:"
            "wheel=%ld:mouse=%ld:w2d=%lld/%lld:full=%ld:delta=%ld:scroll=%ld:checks=%ld:bad=%ld:wkcalls=%ld:wkmax=%lld",
            mode().legacy ? "legacy" : "fast", stats.paints, stats.needs, stats.pixels, stats.clearUs, stats.paintUs,
            stats.swizzleUs, stats.postUs, stats.paints ? stats.totalUs / stats.paints : 0LL, stats.maxUs,
            stats.maxLatencyUs, stats.wheels, stats.mice, stats.w2dCount ? stats.w2dSumUs / stats.w2dCount : 0LL,
            stats.w2dMaxUs, stats.fulls, stats.deltas, stats.scrolls, stats.checks, stats.bad, stats.wkCalls, stats.wkMaxUs);
        unsigned long long wk[8] = { 0 };
        WKPageGetPaintStats(wk, 8);
        say("INFO:WKPAINT:calls=%llu:total=%llu:wrap=%llu:endprint=%llu:area=%llu:snapshot=%llu:draw=%llu:whole=%llu", wk[0] - lastWk[0],
            wk[1] - lastWk[1], wk[2] - lastWk[2], wk[3] - lastWk[3], wk[4] - lastWk[4], wk[5] - lastWk[5], wk[6] - lastWk[6], wk[7] - lastWk[7]);
        memcpy(lastWk, wk, sizeof(wk));
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

// Rectangles a plan may hold (damage.rs MAX_RECTS).
constexpr int kMaxRects = 8;

// ---- the check -------------------------------------------------------------------------------
//
// A delta is only right if WebKit's updates mean what damage.rs takes them to mean (which way an
// offset points, that the rows a scroll exposes are among the repainted rectangles). The host tests
// hold that against a model read from WebKit's source; this holds it against the engine. In a probe
// image the paint keeps `shadow.picture`, what the window should show: the whole frame as sent, then
// each delta applied the way the window applies it. Every few deltas a whole paint of the view is
// taken from the backing store and the two are compared; a difference is a FAIL line (a harness
// fails on it), and the next paint is a whole one, which heals the picture. The cost is a whole
// paint now and then and is kept out of the INFO:PAINT stage times.
struct Shadow {
    std::vector<uint8_t> picture;
    std::vector<uint8_t> whole;     // a whole paint, BGRA
    std::vector<uint8_t> converted; // ... and as the window gets it
    long deltasSinceCheck { 0 };
};
Shadow shadow;
constexpr long kCheckEvery = 30;

// The area moved by (dx, dy) as the window moves it: what was inside goes on, what it uncovers keeps
// its old pixels.
void shadowScroll(int sx, int sy, int sw, int sh, int dx, int dy)
{
    const int x0 = std::max(sx, 0), y0 = std::max(sy, 0), x1 = std::min(sx + sw, E.width), y1 = std::min(sy + sh, E.height);
    const int tx0 = std::max(x0 + dx, x0), ty0 = std::max(y0 + dy, y0), tx1 = std::min(x1 + dx, x1), ty1 = std::min(y1 + dy, y1);
    if (tx1 <= tx0 || ty1 <= ty0)
        return;
    const size_t stride = static_cast<size_t>(E.width) * 4;
    const size_t rowBytes = static_cast<size_t>(tx1 - tx0) * 4;
    for (int i = 0; i < ty1 - ty0; ++i) {
        const int row = dy > 0 ? ty1 - ty0 - 1 - i : i;
        memmove(shadow.picture.data() + static_cast<size_t>(ty0 + row) * stride + static_cast<size_t>(tx0) * 4,
            shadow.picture.data() + static_cast<size_t>(ty0 + row - dy) * stride + static_cast<size_t>(tx0 - dx) * 4, rowBytes);
    }
}

void shadowRect(const Damage& r)
{
    const size_t stride = static_cast<size_t>(E.width) * 4;
    for (int y = r.y0; y < r.y1; ++y)
        memcpy(shadow.picture.data() + y * stride + static_cast<size_t>(r.x0) * 4, E.pixels.data() + y * stride + static_cast<size_t>(r.x0) * 4,
            static_cast<size_t>(r.x1 - r.x0) * 4);
}

// A whole paint against the shadow; true when they agree.
bool shadowAgrees()
{
    const size_t bytes = static_cast<size_t>(E.width) * E.height * 4;
    shadow.whole.assign(bytes, 0xff);
    shadow.converted.assign(bytes, 0xff);
    WKPagePaint(E.page, shadow.whole.data(), WKSizeMake(E.width, E.height), WKRectMake(0, 0, E.width, E.height));
    convertRect(shadow.whole.data(), shadow.converted.data(), E.width * 4, Damage { 0, 0, E.width, E.height });
    ++stats.checks;
    if (shadow.converted == shadow.picture)
        return true;
    long differing = 0;
    int firstX = -1, firstY = -1;
    for (size_t i = 0; i < bytes; i += 4) {
        if (memcmp(shadow.converted.data() + i, shadow.picture.data() + i, 4)) {
            if (!differing) {
                firstX = static_cast<int>((i / 4) % E.width);
                firstY = static_cast<int>((i / 4) / E.width);
            }
            ++differing;
        }
    }
    ++stats.bad;
    say("FAIL:PAINT:MISMATCH:pixels=%ld:first=%d,%d:view=%dx%d", differing, firstX, firstY, E.width, E.height);
    return false;
}

void requestPaint(long long now);

void paintNow(void*)
{
    E.paintScheduled = false;
    if (!E.page || E.width <= 0 || E.height <= 0)
        return;
    const bool legacy = mode().legacy;
    const long long t0 = nowUs();
    const size_t bytes = static_cast<size_t>(E.width) * E.height * 4;

    int scroll[6] = { 0 };
    int rects[kMaxRects * 4] = { 0 };
    int count = 0;
    int plan = lzw_paint_plan(scroll, rects, kMaxRects, &count);
    if (legacy && plan != LZW_PAINT_NOTHING)
        plan = LZW_PAINT_FULL;
    if (paint.width != E.width || paint.height != E.height || paint.target.size() != bytes) {
        // A new size: both bitmaps start white (what the page does not cover is white, never
        // see-through), once and not on every paint, and the window gets the whole of it.
        paint.target.assign(bytes, 0xff);
        E.pixels.assign(bytes, 0xff);
        paint.width = E.width;
        paint.height = E.height;
        plan = LZW_PAINT_FULL;
    }
    const long long pendingSince = paint.pendingSinceUs;
    paint.pendingSinceUs = 0;
    if (plan == LZW_PAINT_NOTHING)
        return;
    paint.lastStartUs = t0;

    long long painted = 0;
    long long t1 = t0, t2 = t0, t3 = t0;
    bool refused = false;
    bool verifyDue = false;
    if (plan == LZW_PAINT_FULL) {
        const Damage whole { 0, 0, E.width, E.height };
        if (legacy)
            memset(paint.target.data(), 0xff, bytes);
        t1 = nowUs();
        WKPagePaint(E.page, paint.target.data(), WKSizeMake(E.width, E.height),
            WKRectMake(0, 0, E.width, E.height));
        t2 = nowUs();
        ++stats.wkCalls;
        stats.wkMaxUs = std::max(stats.wkMaxUs, t2 - t1);
        if (legacy)
            convertBytes(paint.target.data(), E.pixels.data(), bytes);
        else
            convertRect(paint.target.data(), E.pixels.data(), E.width * 4, whole);
        t3 = nowUs();
        lzw_ui_frame(E.pixels.data(), E.width, E.height);
        painted = static_cast<long long>(E.width) * E.height;
        ++stats.fulls;
        if (mode().verify) {
            shadow.picture = E.pixels;
            shadow.deltasSinceCheck = 0;
        }
    } else {
        // The rectangles of the plan, painted from WebKit's backing store (which has already moved
        // what scrolled) and converted where they are.
        long long paintUs = 0, convertUs = 0;
        for (int i = 0; i < count; ++i) {
            const Damage r { rects[4 * i], rects[4 * i + 1], rects[4 * i] + rects[4 * i + 2], rects[4 * i + 1] + rects[4 * i + 3] };
            const long long a = nowUs();
            WKPagePaint(E.page, paint.target.data(), WKSizeMake(E.width, E.height),
                WKRectMake(r.x0, r.y0, r.x1 - r.x0, r.y1 - r.y0));
            const long long b = nowUs();
            convertRect(paint.target.data(), E.pixels.data(), E.width * 4, r);
            const long long c = nowUs();
            paintUs += b - a, convertUs += c - b;
            ++stats.wkCalls;
            stats.wkMaxUs = std::max(stats.wkMaxUs, b - a);
            painted += static_cast<long long>(r.x1 - r.x0) * (r.y1 - r.y0);
        }
        const bool moved = scroll[2] > 0 && scroll[3] > 0 && (scroll[4] || scroll[5]);
        t2 = t1 + paintUs;
        t3 = t2 + convertUs;
        refused = lzw_ui_frame_delta(E.pixels.data(), E.width, E.height, moved ? scroll : nullptr, rects, count) != 0;
        ++stats.deltas;
        if (moved)
            ++stats.scrolls;
        if (mode().verify && !refused && shadow.picture.size() == bytes) {
            if (moved)
                shadowScroll(scroll[0], scroll[1], scroll[2], scroll[3], scroll[4], scroll[5]);
            for (int i = 0; i < count; ++i)
                shadowRect(Damage { rects[4 * i], rects[4 * i + 1], rects[4 * i] + rects[4 * i + 2], rects[4 * i + 1] + rects[4 * i + 3] });
            verifyDue = ++shadow.deltasSinceCheck >= kCheckEvery || stats.checks == 0;
        }
    }
    const long long t4 = nowUs();
    if (refused) {
        // The window has nothing to apply it to or is too far behind: the next paint is a whole one.
        lzw_paint_full();
        requestPaint(t4);
    }

    ++stats.paints;
    stats.pixels += painted;
    stats.clearUs += t1 - t0, stats.paintUs += t2 - t1, stats.swizzleUs += t3 - t2, stats.postUs += t4 - t3;
    stats.totalUs += t4 - t0;
    stats.maxUs = std::max(stats.maxUs, t4 - t0);
    if (pendingSince)
        stats.maxLatencyUs = std::max(stats.maxLatencyUs, t0 - pendingSince);
    report(t4);

    if (verifyDue) {
        shadow.deltasSinceCheck = 0;
        if (!shadowAgrees()) {
            lzw_paint_full();
            requestPaint(nowUs());
        }
    }

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

// A report arrived (a rectangle needs display, or an update): counted, and a paint is on its way.
void noteReport(long long now)
{
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

// An update from the page's process (WKPageSetUpdateCallback): what scrolled and what was repainted.
void pageUpdated(WKPageRef, WKRect scrollRect, WKSize scrollOffset, const WKRect* updateRects, size_t count, void*)
{
    int ints[64 * 4];
    int n = 0;
    for (size_t i = 0; i < count && n < 64; ++i, ++n) {
        const WKRect& r = updateRects[i];
        const int x0 = static_cast<int>(floor(r.origin.x)), y0 = static_cast<int>(floor(r.origin.y));
        const int x1 = static_cast<int>(ceil(r.origin.x + r.size.width)), y1 = static_cast<int>(ceil(r.origin.y + r.size.height));
        ints[4 * n] = x0, ints[4 * n + 1] = y0, ints[4 * n + 2] = x1 - x0, ints[4 * n + 3] = y1 - y0;
    }
    if (count > static_cast<size_t>(n)) {
        // More rectangles than a plan can hold: say all of it.
        lzw_paint_full();
    } else {
        lzw_paint_update(static_cast<int>(floor(scrollRect.origin.x)), static_cast<int>(floor(scrollRect.origin.y)),
            static_cast<int>(ceil(scrollRect.size.width)), static_cast<int>(ceil(scrollRect.size.height)),
            static_cast<int>(scrollOffset.width), static_cast<int>(scrollOffset.height), ints, n);
    }
    noteReport(nowUs());
}

} // namespace

void installPaintObserver()
{
    lzw_paint_resize(E.width, E.height);
    // The legacy path wants WebKit's own "the whole view needs display" for any update that scrolled.
    if (!mode().legacy)
        WKPageSetUpdateCallback(E.page, pageUpdated, nullptr);
}

// A caller that names no rectangle (a load finished, the window changed size): all of it.
void schedulePaint()
{
    lzw_paint_resize(E.width, E.height);
    lzw_paint_full();
    requestPaint(nowUs());
}

void viewNeedsDisplay(WKViewRef, WKRect rect, const void*)
{
    if (mode().legacy)
        lzw_paint_full();
    else
    {
        const int x0 = static_cast<int>(floor(rect.origin.x)), y0 = static_cast<int>(floor(rect.origin.y));
        const int x1 = static_cast<int>(ceil(rect.origin.x + rect.size.width)), y1 = static_cast<int>(ceil(rect.origin.y + rect.size.height));
        lzw_paint_rect(x0, y0, x1 - x0, y1 - y0);
    }
    noteReport(nowUs());
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
    lzw_paint_full();
}

} // namespace webkitapp
