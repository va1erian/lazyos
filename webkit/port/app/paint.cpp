// Painting the page into a bitmap for the window (docs/webkit-port-plan.md, W5).

#include <WebKit/WKGeometry.h>
#include <WebKit/WKPage.h>
#include <WebKit/WKPagePrivate.h>
#include <WebKit/WKPagePrivatePlayStation.h>
#include <WebKit/WKRunLoop.h>

#include <string.h>

#include "engine.h"
#include "lazyos_ui.h"

namespace webkitapp {

namespace {

void paintNow(void*)
{
    E.paintScheduled = false;
    if (!E.page || E.width <= 0 || E.height <= 0)
        return;
    const size_t bytes = static_cast<size_t>(E.width) * E.height * 4;
    if (E.pixels.size() != bytes)
        E.pixels.assign(bytes, 0xff);
    // What the page does not cover is white, never see-through.
    memset(E.pixels.data(), 0xff, bytes);
    WKPagePaint(E.page, E.pixels.data(), WKSizeMake(E.width, E.height), WKRectMake(0, 0, E.width, E.height));
    // Skia's N32 is BGRA on this target and premultiplied; the window wants opaque RGBA.
    uint8_t* p = E.pixels.data();
    for (size_t i = 0; i < bytes; i += 4) {
        uint8_t blue = p[i];
        p[i] = p[i + 2];
        p[i + 2] = blue;
        p[i + 3] = 0xff;
    }
    lzw_ui_frame(E.pixels.data(), E.width, E.height);
    if (E.firstPaintMs < 0 && E.committedSincePaint) {
        E.firstPaintMs = elapsedMs();
        say("WEBKIT:W5:INFO:TIME:firstpaint=%ld:sincecommit=%ld:sincenav=%ld", E.firstPaintMs, E.firstPaintMs - E.commitMs,
            E.firstPaintMs - E.navStartMs);
    }
}

} // namespace

// Many rectangles arrive for one change: one paint takes them all.
void schedulePaint()
{
    if (E.paintScheduled)
        return;
    E.paintScheduled = true;
    WKRunLoopCallOnMainThread(paintNow, nullptr);
}

void viewNeedsDisplay(WKViewRef, WKRect, const void*) { schedulePaint(); }

} // namespace webkitapp
