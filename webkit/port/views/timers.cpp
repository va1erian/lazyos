// webkit-views timers: the generic RunLoop's timer tree when timers are stopped and destroyed in another
// order than they were started (patch 0037). Each timer's node sits in a red-black tree whose links are
// CheckedPtrs; before the patch a stopped timer's node kept its links to its old children, so destroying
// those children first made zombies of them and destroying the stopped one then aborted
// (crashDueToCheckedPtrToDeadObject): webd's abort after a web process's proxy was torn down. Builds on WTF
// alone, so it runs before the engine starts.

// The build's options first, as WTF's own config.h takes them, so this file sees WTF as libWTF was built.
#include "cmakeconfig.h"
#include <wtf/Platform.h>
#include <wtf/ExportMacros.h>

#include <stdio.h>

#include <memory>
#include <wtf/RunLoop.h>
#include <wtf/Seconds.h>

namespace {

constexpr int kTimers = 31;

// Starts `kTimers` one-shot timers far in the future (so the tree has inner nodes), stops them in the
// order `stopOrder` gives, then destroys them in the order `destroyOrder` gives. An abort is the failure.
void round(int (*stopOrder)(int), int (*destroyOrder)(int))
{
    std::unique_ptr<RunLoop::Timer> timers[kTimers];
    for (int i = 0; i < kTimers; ++i) {
        timers[i] = std::make_unique<RunLoop::Timer>(Ref { RunLoop::currentSingleton() }, "webkit-views timers"_s, [] { });
        timers[i]->startOneShot(Seconds(1000 + i));
    }
    for (int i = 0; i < kTimers; ++i)
        timers[stopOrder(i)]->stop();
    for (int i = 0; i < kTimers; ++i)
        timers[destroyOrder(i)] = nullptr;
}

int forward(int i) { return i; }
int backward(int i) { return kTimers - 1 - i; }
int middleOut(int i) { return (i * 7) % kTimers; } // 7 and 31 are coprime: every index once

} // namespace

// Every pairing of three orders; prints VIEWS:TIMERS:PASS (the process aborts otherwise).
void runTimersTest()
{
    int (*orders[3])(int) = { forward, backward, middleOut };
    int rounds = 0;
    for (auto* stop : orders) {
        for (auto* destroy : orders) {
            round(stop, destroy);
            ++rounds;
        }
    }
    // Stopped and restarted: a node that left the tree goes back in, then the rest of the tree goes first.
    std::unique_ptr<RunLoop::Timer> kept[kTimers];
    for (int i = 0; i < kTimers; ++i) {
        kept[i] = std::make_unique<RunLoop::Timer>(Ref { RunLoop::currentSingleton() }, "webkit-views timers"_s, [] { });
        kept[i]->startOneShot(Seconds(1000 + i));
    }
    for (int i = 0; i < kTimers; i += 2)
        kept[i]->stop();
    for (int i = 1; i < kTimers; i += 2)
        kept[i] = nullptr;
    for (int i = 0; i < kTimers; i += 2)
        kept[i]->startOneShot(Seconds(2000 + i));
    for (int i = kTimers - 1; i >= 0; --i)
        kept[i] = nullptr;
    printf("VIEWS:TIMERS:rounds=%d:timers=%d\n", rounds + 1, kTimers);
    printf("VIEWS:PASS\n");
    fflush(stdout);
}
