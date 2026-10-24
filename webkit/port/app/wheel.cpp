// Wheel turns and the page's answers to them.
//
// The owner's window shows its picture ahead of the page while the page is busy (a script runs for hundreds
// of milliseconds while a heavy page loads; the wheel waits for it). To know how long to keep it ahead, and
// how much of it the page really did, it numbers its turns (`seq`); this file tells it when the page has
// answered each one and where the page then was (ViewSink::wheelDone).
//
// WebKit says, after every wheel event it is given, sends, drops or has answered, how many it has not had
// an answer for (WKPageSetWheelDoneCallback, patch 0035; 0 when the page's process ended). A turn becomes
// one WebKit event per notch per axis (at most 8 each), so each turn keeps how many of its events are
// unanswered; the oldest turns are answered first, as WebKit answers its events in order. The page's process
// says it scrolled (PageDidScroll, paint.cpp's latest offset) before it answers the event that scrolled it,
// on the same connection, so the offset at an answer is where the answered turns left the page.

#include <WebKit/WKGeometry.h>
#include <WebKit/WKPagePrivatePlayStation.h>

#include <algorithm>

#include "engine.h"

namespace webkitapp {

namespace {

// Turns kept per view. A page that never answers (a hung page process) and an owner that keeps turning the
// wheel would grow the list for good: past this the oldest turn is folded into the next one, whose answer
// then answers both (an answer always answers every turn before it).
constexpr size_t kMaxTurns = 256;

// WebKit has answered all but `pending` of the events given to it: the turns that are answered in full are
// told to the owner, the newest numbered one with the page's offset.
void settle(View& v, size_t pending)
{
    v.wheelPending = pending;
    size_t answered = v.wheelGiven > pending ? v.wheelGiven - pending : 0;
    v.wheelGiven = std::min(v.wheelGiven, pending);
    uint64_t done = 0;
    while (!v.wheelTurns.empty()) {
        View::WheelTurn& turn = v.wheelTurns.front();
        const size_t taken = std::min(answered, turn.events);
        turn.events -= taken;
        answered -= taken;
        // A turn still being given may get more events: it is not answered yet, nor is anything after it.
        if (turn.events || turn.giving)
            break;
        if (turn.seq)
            done = turn.seq;
        v.wheelTurns.pop_front();
    }
    if (!done)
        return;
    int x = 0, y = 0;
    latestScroll(v, x, y);
    v.sink->wheelDone(done, x, y);
}

void wheelAnswered(WKPageRef page, size_t pending, void*)
{
    if (View* v = viewOfPage(page))
        settle(*v, pending);
}

// One of WebKit's wheel events, counted before it is given: WebKit tells the new count from inside the call.
void give(View& v, int x, int y, int dx, int dy, uint32_t mods)
{
    v.wheelTurns.back().events++;
    v.wheelGiven++;
    WKPageHandleWheelEvent(v.page, WKWheelEventMake(kWKEventWheel, WKPointMake(x, y), WKSizeMake(dx, dy), WKSizeMake(0, 0), mods));
}

} // namespace

void installWheelObserver(View& v)
{
    // Process-wide in WebKit (one callback, whichever page registered last): views are found by their page.
    static bool registered = false;
    if (registered)
        return;
    registered = true;
    WKPageSetWheelDoneCallback(v.page, wheelAnswered, nullptr);
}

void View::wheel(int x, int y, int ticksX, int ticksY, uint32_t mods, uint64_t seq)
{
    notePointer(*this, true);
    if (wheelTurns.size() >= kMaxTurns) {
        const size_t events = wheelTurns.front().events;
        wheelTurns.pop_front();
        wheelTurns.front().events += events;
    }
    wheelTurns.push_back({ seq, 0, true });
    // One event per notch: each is a line step of 40 pixels, and WebKit adds up the steps of events it merges.
    for (int i = 0; i < (ticksY < 0 ? -ticksY : ticksY) && i < 8; ++i)
        give(*this, x, y, 0, ticksY < 0 ? -1 : 1, mods);
    // xui's positive x is to the right; WebKit's is to the left (as GTK's GDK_SCROLL_LEFT is +1).
    for (int i = 0; i < (ticksX < 0 ? -ticksX : ticksX) && i < 8; ++i)
        give(*this, x, y, ticksX < 0 ? 1 : -1, 0, mods);
    wheelTurns.back().giving = false;
    // A turn of no events, or whose events were all dropped (no page process), is answered now.
    settle(*this, wheelPending);
}

} // namespace webkitapp
