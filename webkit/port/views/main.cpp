// webkit-views: the engine's views, headless (docs/webd-plan.md, P1). Two owners of the engine's core
// (webkit/port/app: engine, clients, policy, uiclient, notify, blocking, paint, children, log) without
// the C ABI of lazyos_ui.h or a window: this file is the HostEnv and the ViewSinks.
//
//   webkit-views light   one view, a non-persistent store, no features: a "light view" (docs/webd-plan.md 3.6)
//   webkit-views two     two views at once, one with a profile directory and one without
//   webkit-views twoeph  two views at once, neither with a profile directory
//   webkit-views share   two views of the same profile directory; the first is destroyed, the second goes on
//   webkit-views group      two views of one group and no profile: ONE web process; the first is destroyed and
//                           the second, still on that process, goes on; a third joins the group after the first
//                           is gone (still one process); recover on a shared process is refused (nothing
//                           happens to the views); the owner then destroys the views (the last one ends the
//                           process) and a new view of the group gets a process of its own again
//   webkit-views groupprof  the same two views with a profile directory
//   webkit-views groupmix   the same group number but different profiles: two web processes (not shared)
//   webkit-views cycle   twelve views made, shown and destroyed one after the other
//   webkit-views rush    twelve views made and destroyed at once, before their pages are up
//   webkit-views timers  no view: WTF's run loop timers stopped and destroyed in other orders than they were
//                        started (patch 0037; a stopped timer's tree node held its old children)
//   webkit-views churn   the cycle with two hundred views: each end of a page's process closes an IPC
//                        connection while the other side may still be writing to it (patch 0036)
// The first view of the pairs has every feature (the clients of a full browser view), the second none.
//
// Each view loads a data: page of its own colour and title and must end up painting that colour and no
// other, whatever the other view does; then the first is destroyed and the second, still alive, loads
// another page and paints it. The engine processes the launcher started are counted (the answer to
// "does a light view need a NetworkProcess", docs/webd-plan.md L1), and a destroyed view's WebProcess must
// end. One line per fact, after "VIEWS:":
//   LAUNCH:web=<n>:network=<n>   the launches seen when the pages were up
//   PAGE:<name>:title=<t>:colour=<c>:ms=<n>   a view showed its page
//   PASS / FAIL:<what>
// Exit status 0 on PASS.

#include <WebKit/WKRunLoop.h>

#include <ctype.h>
#include <pthread.h>
#include <signal.h>
#include <ucontext.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <atomic>
#include <memory>
#include <string>
#include <time.h>
#include <unistd.h>
#include <vector>

#include "engine.h"
#include "lazyos_ui.h"

using namespace webkitapp;

void runTimersTest(); // timers.cpp

extern "C" const char __ehdr_start;

namespace {

// A crash leaves no message: print the signal and every word of the first stack kilobytes that points
// into the program's code, for `addr2line -f -C -e <the unstripped program> <at>` (the program is not
// position independent). The same idea as main.cpp's handler, smaller.
void reportCrash(int signal, siginfo_t*, void* context)
{
    const unsigned long base = reinterpret_cast<unsigned long>(&__ehdr_start);
    auto* uc = static_cast<ucontext_t*>(context);
    const unsigned long* words = reinterpret_cast<const unsigned long*>(uc->uc_mcontext.gregs[REG_RSP]);
    char line[64];
    int printed = 0;
    int length = snprintf(line, sizeof line, "VIEWS:CRASH:signal=%d:base=%lx\n", signal, base);
    ssize_t ignored = write(STDOUT_FILENO, line, static_cast<size_t>(length));
    for (int i = 0; i < 4096 && printed < 40; ++i) {
        const unsigned long word = words[i];
        if (word > base && word < base + 0x8000000UL) {
            length = snprintf(line, sizeof line, "VIEWS:CRASH:at=%lx\n", word);
            ignored = write(STDOUT_FILENO, line, static_cast<size_t>(length));
            ++printed;
        }
    }
    (void)ignored;
    _exit(134);
}

char crashStack[64 * 1024];

long monotonicMs()
{
    timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec * 1000L + now.tv_nsec / 1000000L;
}

// Written from the launcher's and reapers' threads (through HostEnv::say), read on the main thread.
struct Counters {
    std::atomic<int> web { 0 };
    std::atomic<int> network { 0 };
    std::atomic<int> webExits { 0 };
};
Counters launches;

class Env final : public HostEnv {
public:
    int features() override { return 0; }
    void say(const char* line) override
    {
        if (!strncmp(line, "PROCESS:LAUNCH:web:", 19) && !strstr(line, "FAIL"))
            ++launches.web;
        else if (!strncmp(line, "PROCESS:LAUNCH:network:", 23) && !strstr(line, "FAIL"))
            ++launches.network;
        else if (!strncmp(line, "PROCESS:EXIT:web:", 17))
            ++launches.webExits;
        printf("VIEWS:%s\n", line);
        fflush(stdout);
    }
    bool probe() override { return true; }
    int userMemory(uint64_t*, uint64_t*) override { return 1; }
};

// The colour a frame is mostly made of: 'r', 'g', 'b', 'w' (white), '?' (none of them covers half).
char dominant(const uint8_t* rgba, int width, int height)
{
    long counts[4] = { 0, 0, 0, 0 };
    for (long i = 0, n = static_cast<long>(width) * height; i < n; ++i) {
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
    const long total = static_cast<long>(width) * height;
    const char names[4] = { 'r', 'g', 'b', 'w' };
    for (int i = 0; i < 4; ++i) {
        if (counts[i] * 2 > total)
            return names[i];
    }
    return '?';
}

// A view's owner: remembers what the page said and what the pixels were, and every colour it ever
// painted (a view must never show the other view's page).
class Probe final : public ViewSink {
public:
    explicit Probe(const char* name)
        : name(name)
    {
    }
    void paintUpdate(int, int, int, int, int, int, const int*, int) override { ++updates; }
    void state(const ViewState& s) override
    {
        title = s.title;
        finished = s.finished;
        error = s.error;
        crashed = s.crashed;
        crashedEver = crashedEver || s.crashed; // a fast reload must not hide a crash from the 100 ms tick
    }
    int paintPlan(int*, int*, int, int* count) override
    {
        *count = 0;
        return kPaintFull;
    }
    void frame(const uint8_t* rgba, int width, int height) override
    {
        ++frames;
        last = dominant(rgba, width, height);
        seen += last;
    }

    const char* name;
    std::string title;
    std::string error;
    int finished { 0 };
    bool crashed { false };
    bool crashedEver { false };
    int frames { 0 };
    int updates { 0 }; // updates from the page's process that reached this view (paint.cpp, viewOfPage)
    char last { '?' };
    std::string seen; // the dominant colour of every frame, in order
};

std::string encode(const std::string& text)
{
    std::string out;
    for (unsigned char c : text) {
        if (isalnum(c) || c == '-' || c == '_' || c == '.')
            out += static_cast<char>(c);
        else {
            char hex[4];
            snprintf(hex, sizeof hex, "%%%02X", c);
            out += hex;
        }
    }
    return out;
}

std::string page(const char* title, const char* colour)
{
    return "data:text/html," + encode(std::string("<title>") + title + "</title><body style=\"margin:0;background:" + colour + "\">");
}

Env env;
Probe first("A"), second("B"), third("C"), fourth("D");
int firstId = 0, secondId = 0, thirdId = 0, fourthId = 0;
bool extended = false; // group: a third view joins, recover is refused, the group is rebuilt
bool mixed = false; // the same group number, different profiles
bool bothViews = false;
int phase = 0;
long started = 0, phaseStarted = 0;
// The cycle mode: a view at a time, N in all (churn: many, for the races of a process's end).
constexpr int kCycles = 12;
constexpr int kChurnCycles = 200;
int cycles = kCycles;
bool cycling = false;
bool rush = false; // destroy a view at once, before its page is up
bool sameProfile = false;
bool grouped = false; // the two views are one group: they must share a single web process
int cycleIndex = 0;
int cycleView = 0;
std::unique_ptr<Probe> cycleProbe;
const char* const cycleColours[3] = { "red", "green", "blue" };
const char cycleLetters[3] = { 'r', 'g', 'b' };
std::atomic<bool> done { false };
bool passed = false;

void finish(const char* verdict, const char* detail)
{
    if (done)
        return;
    done = true;
    passed = !strcmp(verdict, "PASS");
    printf("VIEWS:LAUNCH:web=%d:network=%d\n", launches.web.load(), launches.network.load());
    if (detail && *detail)
        printf("VIEWS:%s:%s\n", verdict, detail);
    else
        printf("VIEWS:%s\n", verdict);
    fflush(stdout);
    WKRunLoopStopMain();
}

bool shows(Probe& probe, const char* title, char colour)
{
    return probe.finished >= 1 && probe.title == title && probe.last == colour;
}

// Never a colour that is another view's page.
bool clean(Probe& probe, const char* forbidden)
{
    for (char c : probe.seen) {
        if (strchr(forbidden, c))
            return false;
    }
    return true;
}

void step(void*)
{
    if (done)
        return;
    const long now = monotonicMs();
    if (now - started > 90000 + cycles * 500L) {
        char detail[512];
        snprintf(detail, sizeof detail, "timeout in phase %d: A title='%s' frames=%d seen=%s finished=%d, B title='%s' frames=%d seen=%s finished=%d err='%s'",
            phase, first.title.c_str(), first.frames, first.seen.c_str(), first.finished, second.title.c_str(), second.frames, second.seen.c_str(), second.finished, second.error.c_str());
        finish("FAIL", detail);
        return;
    }
    if (first.crashedEver || second.crashedEver || third.crashedEver || fourth.crashedEver) {
        finish("FAIL", "a page's process crashed");
        return;
    }
    if (cycling && phase != 2) {
        if (!cycleProbe) {
            ViewConfig config;
            config.width = 240;
            config.height = 160;
            cycleProbe = std::make_unique<Probe>("cycle");
            View* made = createView(config, cycleProbe.get());
            if (!made) {
                finish("FAIL", "a view in the cycle was refused");
                return;
            }
            cycleView = made->id;
            made->load(page("Cycle", cycleColours[cycleIndex % 3]));
            if (!rush)
                return;
        }
        if (rush || (cycleProbe->finished >= 1 && cycleProbe->last == cycleLetters[cycleIndex % 3])) {
            destroyView(cycleView);
            cycleProbe.reset();
            if (++cycleIndex == cycles) {
                phase = 2;
                phaseStarted = now;
            }
        }
        return;
    }
    if (phase == 2) {
        // Every destroyed view's WebProcess must end.
        if (launches.webExits >= cycles) {
            printf("VIEWS:CYCLED:%d:web launches=%d exits=%d\n", cycles, launches.web.load(), launches.webExits.load());
            finish("PASS", "");
        } else if (now - phaseStarted > 20000) {
            char detail[128];
            snprintf(detail, sizeof detail, "only %d of %d web processes ended", launches.webExits.load(), cycles);
            finish("FAIL", detail);
        }
        return;
    }
    if (phase == 0) {
        const bool upA = !bothViews || shows(first, "Alpha", 'r');
        if (upA && shows(second, "Beta", 'b')) {
            printf("VIEWS:LAUNCH:web=%d:network=%d\n", launches.web.load(), launches.network.load());
            printf("VIEWS:UPDATES:A=%d:B=%d\n", first.updates, second.updates);
            if (grouped && launches.web != 1) {
                finish("FAIL", "the views of a group did not share one web process");
                return;
            }
            if (mixed && launches.web < 2) {
                finish("FAIL", "views of one group number but two profiles shared a web process");
                return;
            }
            if (!grouped && bothViews && launches.web < 2) {
                finish("FAIL", "two views of no group shared a web process");
                return;
            }
            if (first.updates + second.updates == 0) {
                // Every frame so far came from the load finishing: the page callbacks never found a view.
                finish("FAIL", "no update from a page's process reached a view");
                return;
            }
            if (bothViews)
                printf("VIEWS:PAGE:A:title=Alpha:colour=red:ms=%ld\n", now - started);
            printf("VIEWS:PAGE:B:title=Beta:colour=blue:ms=%ld\n", now - started);
            fflush(stdout);
            if (bothViews && (!clean(first, "bg") || !clean(second, "rg"))) {
                finish("FAIL", "a view painted the other view's colour");
                return;
            }
            phase = 1;
            phaseStarted = now;
            if (bothViews) {
                destroyView(firstId);
                printf("VIEWS:DESTROYED:A\n");
            }
            if (View* v = viewById(secondId))
                v->load(page("Gamma", "green"));
        }
        return;
    }
    if (phase == 1 && second.finished >= 2 && second.title == "Gamma" && second.last == 'g') {
        // A's WebProcess ended with its view (its view was the only one that used it).
        if (grouped && (launches.webExits != 0 || launches.web != 1)) {
            // The process stays: the second view is on it (a launch counts when it starts, a new one
            // would have been counted by now).
            finish("FAIL", "destroying one view of a group ended the process the other one uses");
            return;
        }
        if (bothViews && !sameProfile && !grouped && launches.webExits < 1) {
            if (now - phaseStarted > 20000)
                finish("FAIL", "the destroyed view's web process did not end");
            return;
        }
        printf("VIEWS:PAGE:B:title=Gamma:colour=green:ms=%ld\n", now - phaseStarted);
        if (!extended) {
            finish("PASS", "");
            return;
        }
        // A third view joins the group whose first member is gone: it must find the second.
        ViewConfig joining;
        joining.width = 320;
        joining.height = 200;
        joining.group = 1;
        View* made = createView(joining, &third);
        if (!made) {
            finish("FAIL", "a view could not join the group");
            return;
        }
        thirdId = made->id;
        made->load(page("Delta", "red"));
        phase = 3;
        phaseStarted = now;
        return;
    }
    if (phase == 3 && shows(third, "Delta", 'r')) {
        if (launches.web != 1) {
            finish("FAIL", "a view that joined the group started a web process of its own");
            return;
        }
        printf("VIEWS:PAGE:C:title=Delta:colour=red:ms=%ld\n", now - phaseStarted);
        // The owner asks to recover the second view: refused, the process is shared; it is a no-op.
        viewById(secondId)->recover(true);
        // It recovers the group the way the engine allows: destroy the views (the last one ends the
        // process), then make a new one.
        destroyView(secondId);
        destroyView(thirdId);
        phase = 4;
        phaseStarted = now;
        return;
    }
    if (phase == 4 && launches.webExits >= 1) {
        printf("VIEWS:GROUP:EMPTY:ms=%ld\n", now - phaseStarted);
        ViewConfig again;
        again.width = 320;
        again.height = 200;
        again.group = 1;
        View* made = createView(again, &fourth);
        if (!made) {
            finish("FAIL", "a view could not rebuild the group");
            return;
        }
        fourthId = made->id;
        made->load(page("Epsilon", "green"));
        phase = 5;
        phaseStarted = now;
        return;
    }
    if (phase == 5 && shows(fourth, "Epsilon", 'g')) {
        printf("VIEWS:GROUP:REBUILT:web launches=%d\n", launches.web.load());
        if (launches.web != 2) {
            finish("FAIL", "the rebuilt group did not start exactly one new web process");
            return;
        }
        finish("PASS", "");
        return;
    }
    if (phase >= 3 && now - phaseStarted > 30000) {
        const char* const why[] = { "the joining view never painted", "the group's process did not end with its last view",
            "the rebuilt group never painted" };
        finish("FAIL", why[phase - 3]);
    }
}

void* ticker(void*)
{
    while (!done) {
        usleep(100 * 1000);
        WKRunLoopCallOnMainThread(step, nullptr);
    }
    return nullptr;
}

} // namespace

int main(int argc, char** argv)
{
    grouped = argc > 1 && (!strcmp(argv[1], "group") || !strcmp(argv[1], "groupprof"));
    extended = argc > 1 && !strcmp(argv[1], "group");
    mixed = argc > 1 && !strcmp(argv[1], "groupmix");
    const bool ephemeralPair = argc > 1 && (!strcmp(argv[1], "twoeph") || !strcmp(argv[1], "group"));
    rush = argc > 1 && !strcmp(argv[1], "rush");
    const bool churn = argc > 1 && !strcmp(argv[1], "churn");
    cycling = rush || churn || (argc > 1 && !strcmp(argv[1], "cycle"));
    if (churn)
        cycles = kChurnCycles;
    sameProfile = argc > 1 && (!strcmp(argv[1], "share") || !strcmp(argv[1], "groupprof"));
    bothViews = argc > 1 && (!strcmp(argv[1], "two") || ephemeralPair || sameProfile || mixed);
    setvbuf(stdout, nullptr, _IOLBF, 0);
    signal(SIGPIPE, SIG_IGN);
    stack_t alternate = { };
    alternate.ss_sp = crashStack;
    alternate.ss_size = sizeof crashStack;
    sigaltstack(&alternate, nullptr);
    struct sigaction action = { };
    action.sa_sigaction = reportCrash;
    action.sa_flags = SA_SIGINFO | SA_ONSTACK | SA_RESETHAND;
    for (int number : { SIGABRT, SIGSEGV, SIGILL, SIGTRAP, SIGBUS })
        sigaction(number, &action, nullptr);
    WKRunLoopInitializeMain();
    if (argc > 1 && !strcmp(argv[1], "timers")) {
        runTimersTest();
        _exit(0);
    }
    const std::string profile = bothViews && !ephemeralPair ? "/tmp/webkit-views-profile" : "";
    if (!startHost(&env, profile)) {
        printf("VIEWS:FAIL:the host did not start\n");
        _exit(1);
    }
    ViewConfig lightConfig;
    lightConfig.width = 320;
    lightConfig.height = 200;
    if (bothViews) {
        ViewConfig full = lightConfig;
        full.profile = profile;
        full.group = grouped || mixed ? 1 : 0;
        // Every client a browser view has: the page UI, dialogs, downloads, policy, notifications and blocking.
        full.features = LZW_FEATURE_PAGE_UI | LZW_FEATURE_DIALOGS | LZW_FEATURE_DOWNLOADS | LZW_FEATURE_POLICY
            | LZW_FEATURE_NOTIFICATIONS | LZW_FEATURE_BLOCKING;
        View* a = createView(full, &first);
        if (!a) {
            printf("VIEWS:FAIL:view A\n");
            _exit(1);
        }
        firstId = a->id;
    }
    ViewConfig second_config = lightConfig;
    if (sameProfile)
        second_config.profile = profile;
    second_config.group = grouped || mixed ? 1 : 0;
    View* b = cycling ? nullptr : createView(second_config, &second);
    if (!b && !cycling) {
        printf("VIEWS:FAIL:view B\n");
        _exit(1);
    }
    secondId = b ? b->id : 0;
    started = monotonicMs();
    // The loads go through the run loop, as the owner's calls do.
    onMain([] {
        if (cycling)
            return;
        if (bothViews)
            viewById(firstId)->load(page("Alpha", "red"));
        viewById(secondId)->load(page("Beta", "blue"));
    });
    pthread_t thread;
    pthread_create(&thread, nullptr, ticker, nullptr);
    WKRunLoopRunMain();
    const int surviving = shutdownChildren();
    printf("VIEWS:CHILDREN:left=%d\n", surviving);
    fflush(stdout);
    _exit(passed && surviving == 0 ? 0 : 1);
}
