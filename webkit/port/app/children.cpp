// The engine processes (issue #862): what the launcher tells us about them, and how they are ended.
//
// WebKit's launcher (Source/WebKit/UIProcess/Launcher/ProcessLauncherLazyOS.cpp) calls
// lzw_launcher_event for every process it starts, collects and kills, so no WebProcess or
// NetworkProcess comes or goes without a line, and the process a NetworkProcess replaced (WebKit's 6 s
// responsiveness timeout kills the busy one and starts another) is known here too. Shutdown works
// from that list alone, and signals through the launcher (`lzw_launcher_signal`, under its lock): a
// process that was collected, whose id may belong to another by now, is never named.
//
// Markers (after the window's prefix):
//   PROCESS:LAUNCH:<web|network>:<pid>              the launcher forked it
//   PROCESS:LAUNCH:<type>:FAIL:errno=<n>            fork failed
//   PROCESS:EXIT:<type>:<pid>:status=<n>|signal=<n> it ended and was collected
//   PROCESS:EXIT:<type>:<pid>:gone                  the kernel has no such id any more (the launcher's
//                                                   thread could not wait for it, issue #859)
//   PROCESS:TERMINATE:<type>:<pid>:group=<e>:pid=<e> WebKit's terminate(): errno of kill(-pid) and of
//                                                   kill(pid) (-1: not tried), 0 when it was sent
//   PROCESS:KEPT:<pid>:<n>                          patch 0021 keeps the dead web process's proxy
//   CHILD:<name>:<pid>:GONE|ALIVE:term=<rc>:kill=<rc>:wait=<rc>
//                                                   the end: each rc is 0, or the errno negated; wait
//                                                   is the pid it reaped, 0 for an id the kernel no
//                                                   longer has, or -1 for one still there. During the
//                                                   shutdown a child's end is this line, not an EXIT.

#include <signal.h>
#include <time.h>
#include <unistd.h>

#include <atomic>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include <WebKit/WKRunLoop.h>

#include "engine.h"
#include "lazyos_ui.h"

namespace webkitapp {

namespace {

// An engine process the launcher started and has not reported ended.
struct Child {
    int pid { 0 };
    std::string type;
    // WebKit's terminate() signalled it: its collection is awaited (see `pumpReaping`).
    bool terminating { false };
};

// Shutdown's view of one process.
struct Target {
    std::string name;
    int pid { 0 };
    bool gone { false };
    int term { 0 };
    int kill { 0 };
    int wait { -1 };
};

std::mutex registryLock;
std::vector<Child> registry;
// How many processes of each type the launcher has started (a launch of a reused id is a new process).
unsigned webLaunches { 0 };
unsigned networkLaunches { 0 };
// The shutdown is under way: a child's end is recorded in `shutdownTargets` and printed with its
// CHILD line, not as a PROCESS:EXIT.
bool shuttingDown { false };
std::vector<Target> shutdownTargets;

constexpr long kPumpStepMs = 100;
constexpr long kPumpGiveUpMs = 60000;

constexpr long kGraceMs = 2000;
constexpr long kGiveUpMs = 10000;
constexpr long kStepMs = 100;
constexpr int kMaxChildren = 32;

long monotonicMs()
{
    timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec * 1000L + now.tv_nsec / 1000000L;
}

unsigned& launchCounter(const std::string& type)
{
    return type == "web" ? webLaunches : networkLaunches;
}

// Sends `signal` to every live child through the launcher and files each answer in the matching target
// as its `term` or `kill` code.
void signalAll(int signal, bool group)
{
    int pids[kMaxChildren];
    int codes[kMaxChildren];
    const int live = lzw_launcher_signal(signal, group ? 1 : 0, pids, codes, kMaxChildren);
    std::lock_guard<std::mutex> guard(registryLock);
    for (int at = 0; at < live && at < kMaxChildren; ++at) {
        for (Target& target : shutdownTargets) {
            if (target.pid == pids[at])
                (signal == SIGTERM ? target.term : target.kill) = codes[at];
        }
    }
}

bool anyTerminating()
{
    std::lock_guard<std::mutex> guard(registryLock);
    for (const Child& child : registry) {
        if (child.terminating)
            return true;
    }
    return false;
}

void reapTask(void*)
{
    lzw_launcher_reap();
}

std::atomic<bool> pumping { false };

// A process that was sent SIGKILL is not collectable until the kernel has finished with it (a thread
// inside a call ends it when the call returns): collecting it once, as the terminate is reported, finds
// it still there and the next collection was the next process's end, a whole reload later, with its
// memory charged all the while. So while a terminated process is awaited the launcher's collection runs
// on the main thread (the thread that forked it) every 100 ms; WKRunLoop has no timer, a thread posts it.
void pumpReaping()
{
    if (pumping.exchange(true))
        return;
    std::thread([] {
        const long began = monotonicMs();
        while (anyTerminating() && monotonicMs() - began < kPumpGiveUpMs) {
            usleep(kPumpStepMs * 1000);
            WKRunLoopCallOnMainThread(reapTask, nullptr);
        }
        pumping = false;
        // A terminate that came while this thread was ending found `pumping` set and started nothing.
        if (anyTerminating())
            pumpReaping();
    }).detach();
}

} // namespace

bool processLive(const char* type, int pid)
{
    std::lock_guard<std::mutex> guard(registryLock);
    for (const Child& child : registry) {
        if (child.pid == pid && child.type == type)
            return true;
    }
    return false;
}

unsigned processLaunches(const char* type)
{
    std::lock_guard<std::mutex> guard(registryLock);
    return launchCounter(type);
}

int shutdownChildren()
{
    if (E.page && E.context)
        rememberChildren();
    {
        std::lock_guard<std::mutex> guard(registryLock);
        shuttingDown = true;
        for (const Child& child : registry)
            shutdownTargets.push_back({ child.type, child.pid });
    }
    signalAll(SIGTERM, false);

    // A grace period for the processes to flush and exit, then SIGKILL of each group, repeated every
    // step: a thread that is inside the kernel is not killed until the call returns (issue #858).
    // Collecting is the launcher's (under its lock); each child's end arrives as an event.
    const long began = monotonicMs();
    for (;;) {
        lzw_launcher_reap();
        bool all = true;
        {
            std::lock_guard<std::mutex> guard(registryLock);
            for (const Target& target : shutdownTargets)
                all = all && target.gone;
        }
        const long elapsed = monotonicMs() - began;
        if (all || elapsed >= kGiveUpMs)
            break;
        if (elapsed >= kGraceMs)
            signalAll(SIGKILL, true);
        usleep(kStepMs * 1000);
    }

    std::vector<Target> list;
    {
        std::lock_guard<std::mutex> guard(registryLock);
        list = shutdownTargets;
    }
    int left = 0;
    for (const Target& target : list) {
        say("CHILD:%s:%d:%s:term=%d:kill=%d:wait=%d", target.name.c_str(), target.pid, target.gone ? "GONE" : "ALIVE",
            target.term, target.kill, target.wait);
        left += target.gone ? 0 : 1;
    }
    return left;
}

} // namespace webkitapp

extern "C" {

// The launcher's report (ProcessLauncherLazyOS.cpp declares it weak, so the stand-alone programs that
// link WebKit without this file print to stderr instead). Any thread: the launching one, or the
// short-lived reaper of each child.
void lzw_launcher_event(const char* type, int pid, const char* event, int detail, int detail2)
{
    using namespace webkitapp;
    const std::string what(event ? event : "");
    if (what == "launch") {
        {
            std::lock_guard<std::mutex> guard(registryLock);
            // An entry of the same id is a process nobody saw end (its slot was reused): replaced.
            for (size_t at = 0; at < registry.size(); ++at) {
                if (registry[at].pid == pid) {
                    registry.erase(registry.begin() + static_cast<long>(at));
                    break;
                }
            }
            registry.push_back({ pid, type });
            ++launchCounter(type);
        }
        say("PROCESS:LAUNCH:%s:%d", type, pid);
        return;
    }
    if (what == "fail") {
        say("PROCESS:LAUNCH:%s:FAIL:errno=%d", type, detail);
        return;
    }
    if (what == "terminate") {
        {
            std::lock_guard<std::mutex> guard(registryLock);
            for (Child& child : registry) {
                if (child.pid == pid && child.type == type)
                    child.terminating = true;
            }
        }
        pumpReaping();
        say("PROCESS:TERMINATE:%s:%d:group=%d:pid=%d", type, pid, detail, detail2);
        return;
    }
    // exit / signal / vanish: no longer the launcher's to signal, and not ours to end. The launcher
    // reports each child once, under its lock: the reaper's thread and `lzw_launcher_reap` cannot both.
    bool known = false;
    bool quiet = false;
    {
        std::lock_guard<std::mutex> guard(registryLock);
        for (size_t at = 0; at < registry.size(); ++at) {
            if (registry[at].pid == pid && registry[at].type == type) {
                registry.erase(registry.begin() + static_cast<long>(at));
                known = true;
                break;
            }
        }
        if (known && shuttingDown) {
            quiet = true;
            for (Target& target : shutdownTargets) {
                if (target.pid == pid && target.name == type && !target.gone) {
                    target.gone = true;
                    target.wait = what == "vanish" ? 0 : pid;
                }
            }
        }
    }
    if (!known)
        return;
    // WebKit's answers go on naming a process after it ended: nothing may use that id again.
    int expected = pid;
    E.webPid.compare_exchange_strong(expected, 0);
    expected = pid;
    E.networkPid.compare_exchange_strong(expected, 0);
    if (quiet)
        return;
    if (what == "exit")
        say("PROCESS:EXIT:%s:%d:status=%d", type, pid, detail);
    else if (what == "signal")
        say("PROCESS:EXIT:%s:%d:signal=%d", type, pid, detail);
    else
        say("PROCESS:EXIT:%s:%d:gone", type, pid);
}

// WebKit's patch 0021 keeps the proxy of an ended web process alive (a leak of a few kilobytes each, for
// a reason that is not found): the count is evidence of how many a run leaked. WebKit no longer knows
// the process's id when it calls (it is cleared as the process is torn down): the page's last announced
// id is the process that ended.
void lzw_proxy_kept(int pid, int kept)
{
    webkitapp::say("PROCESS:KEPT:%d:%d", pid > 0 ? pid : webkitapp::E.webPidShown, kept);
}

// The ids of the page's WebProcess and of the NetworkProcess as last seen (0: none yet); any thread.
void lzw_engine_children(int* web, int* network)
{
    if (web)
        *web = webkitapp::E.webPid;
    if (network)
        *network = webkitapp::E.networkPid;
}

} // extern "C"
