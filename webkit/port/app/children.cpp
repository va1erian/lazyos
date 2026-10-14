// The engine processes (issue #862): what the launcher tells us about them, and how they are ended.
//
// WebKit's launcher (Source/WebKit/UIProcess/Launcher/ProcessLauncherLazyOS.cpp) calls
// lzw_launcher_event for every process it starts, collects and kills, so no WebProcess or
// NetworkProcess comes or goes without a line, and the process a NetworkProcess replaced (WebKit's 6 s
// responsiveness timeout kills the busy one and starts another) is known here too. Shutdown works
// from that list, not only from the two ids WebKit still answers with at the end.
//
// Markers (after the window's prefix):
//   PROCESS:LAUNCH:<web|network>:<pid>              the launcher forked it
//   PROCESS:LAUNCH:<type>:FAIL:errno=<n>            fork failed
//   PROCESS:EXIT:<type>:<pid>:status=<n>|signal=<n> it ended and was collected
//   PROCESS:EXIT:<type>:<pid>:gone                  the kernel has no such id any more (the launcher's
//                                                   thread could not wait for it, issue #859)
//   PROCESS:TERMINATE:<type>:<pid>:group=<e>:pid=<e> WebKit's terminate(): errno of kill(-pid) and of
//                                                   kill(pid) (-1: not tried), 0 when it was sent
//   CHILD:<name>:<pid>:GONE|ALIVE:term=<rc>:kill=<rc>:wait=<rc>
//                                                   the end: each rc is 0, or the errno negated; wait
//                                                   is the pid it reaped, or the errno negated

#include <errno.h>
#include <signal.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include <mutex>
#include <string>
#include <vector>

#include "engine.h"
#include "lazyos_ui.h"

namespace webkitapp {

namespace {

// An engine process the launcher started and has not reported ended.
struct Child {
    int pid { 0 };
    std::string type;
};

std::mutex registryLock;
std::vector<Child> registry;
// Ids of processes seen to end: WebKit's own answers (`E.webPid`) go on naming them, and the number
// may belong to another process by shutdown. A launch of the same id takes it off again.
std::vector<int> ended;

// Shutdown's view of one process.
struct Target {
    std::string name;
    int pid { 0 };
    bool gone { false };
    int term { 0 };
    int kill { 0 };
    int wait { 0 };
};

constexpr long kGraceMs = 2000;
constexpr long kGiveUpMs = 10000;
constexpr long kStepMs = 100;

long monotonicMs()
{
    timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec * 1000L + now.tv_nsec / 1000000L;
}

// Is the process gone? Collects it when it is ours to collect. One that is not (the launcher's
// thread forked it, issue #859) is gone when the kernel has no such id; a zombie that nobody has
// collected still answers kill(pid, 0), and stays "not gone" until it is.
bool reap(Target& target)
{
    int status = 0;
    const pid_t reaped = waitpid(target.pid, &status, WNOHANG);
    if (reaped == target.pid) {
        target.wait = static_cast<int>(reaped);
        return target.gone = true;
    }
    if (reaped < 0) {
        const int error = errno;
        target.wait = -error;
        if (error == ECHILD && ::kill(target.pid, 0) < 0 && errno == ESRCH)
            target.gone = true;
    }
    return target.gone;
}

// SIGKILL to the process's group (the launcher made each its own) and to the process itself.
int killProcess(const Target& target)
{
    int result = 0;
    if (::kill(-target.pid, SIGKILL) < 0)
        result = -errno;
    if (::kill(target.pid, SIGKILL) < 0 && !result)
        result = -errno;
    return result;
}

// The processes still to end: the launcher's list, and the ids WebKit says that it never told us of.
std::vector<Target> targets()
{
    std::vector<Target> list;
    {
        std::lock_guard<std::mutex> guard(registryLock);
        for (const Child& child : registry)
            list.push_back({ child.type, child.pid });
    }
    auto known = [&list](int pid) {
        for (const Target& target : list) {
            if (target.pid == pid)
                return true;
        }
        return false;
    };
    auto over = [](int pid) {
        std::lock_guard<std::mutex> guard(registryLock);
        for (int id : ended) {
            if (id == pid)
                return true;
        }
        return false;
    };
    const int web = E.webPid, network = E.networkPid;
    if (web > 0 && !known(web) && !over(web))
        list.push_back({ "web", web });
    if (network > 0 && !known(network) && !over(network))
        list.push_back({ "network", network });
    return list;
}

} // namespace

// The processes the launcher's reaper thread cannot collect (until issue #859 a child belongs to the
// thread that forked it, and WebKit forks on this one) become zombies; this thread can collect them.
void reapChildren()
{
    std::vector<Child> list;
    {
        std::lock_guard<std::mutex> guard(registryLock);
        list = registry;
    }
    for (const Child& child : list) {
        int status = 0;
        if (waitpid(child.pid, &status, WNOHANG) != child.pid)
            continue;
        if (WIFSIGNALED(status))
            lzw_launcher_event(child.type.c_str(), child.pid, "signal", WTERMSIG(status), 0);
        else
            lzw_launcher_event(child.type.c_str(), child.pid, "exit", WEXITSTATUS(status), 0);
    }
}

int shutdownChildren()
{
    if (E.page && E.context)
        rememberChildren();
    std::vector<Target> list = targets();
    for (Target& target : list)
        target.term = ::kill(target.pid, SIGTERM) < 0 ? -errno : 0;

    // A grace period for the processes to flush and exit, then SIGKILL, repeated every step: a thread
    // that is inside the kernel is not killed until the call returns (issue #858).
    const long began = monotonicMs();
    for (;;) {
        bool all = true;
        for (Target& target : list) {
            if (!target.gone)
                reap(target);
            all = all && target.gone;
        }
        const long elapsed = monotonicMs() - began;
        if (all || elapsed >= kGiveUpMs)
            break;
        if (elapsed >= kGraceMs) {
            for (Target& target : list) {
                if (!target.gone)
                    target.kill = killProcess(target);
            }
        }
        usleep(kStepMs * 1000);
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
            registry.push_back({ pid, type });
            for (size_t at = 0; at < ended.size(); ++at) {
                if (ended[at] == pid) {
                    ended.erase(ended.begin() + static_cast<long>(at));
                    break;
                }
            }
        }
        say("PROCESS:LAUNCH:%s:%d", type, pid);
        return;
    }
    if (what == "fail") {
        say("PROCESS:LAUNCH:%s:FAIL:errno=%d", type, detail);
        return;
    }
    if (what == "terminate") {
        say("PROCESS:TERMINATE:%s:%d:group=%d:pid=%d", type, pid, detail, detail2);
        return;
    }
    // exit / signal / vanish: no longer the launcher's to signal, and not ours to end. Said once:
    // the main thread (reapChildren) and the launcher's reaper can both find the same process over.
    bool known = false;
    {
        std::lock_guard<std::mutex> guard(registryLock);
        for (size_t at = 0; at < registry.size(); ++at) {
            if (registry[at].pid == pid) {
                registry.erase(registry.begin() + static_cast<long>(at));
                if (ended.size() < 64)
                    ended.push_back(pid);
                known = true;
                break;
            }
        }
    }
    if (!known)
        return;
    if (what == "exit")
        say("PROCESS:EXIT:%s:%d:status=%d", type, pid, detail);
    else if (what == "signal")
        say("PROCESS:EXIT:%s:%d:signal=%d", type, pid, detail);
    else
        say("PROCESS:EXIT:%s:%d:gone", type, pid);
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
