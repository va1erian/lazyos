/*
 * Copyright (C) 2026 LazyOS contributors
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions
 * are met:
 * 1. Redistributions of source code must retain the above copyright
 *    notice, this list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright
 *    notice, this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 *
 * THIS SOFTWARE IS PROVIDED BY THE AUTHORS ``AS IS'' AND ANY EXPRESS OR
 * IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF
 * MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO
 * EVENT SHALL THE AUTHORS OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT,
 * INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
 * LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
 * PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
 * LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
 * NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE,
 * EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */

// The process launcher of the LazyOS port (webkit/port/, docs/webkit-port-plan.md W4).
//
// The PlayStation launcher calls a closed library; the GLib one needs GSubprocess.
// This one is plain POSIX: a SOCK_SEQPACKET socketpair, fork, and exec of the
// WebProcess or NetworkProcess with the client end open across the exec (both ends are
// close-on-exec; only the forked child clears the flag on the client end, issue #889, so no other
// program this process starts meanwhile inherits it), the command line the generic unix
// AuxiliaryProcessMain expects: "<program> <process identifier> <client fd>".
//
// fork and execv rather than posix_spawn: LazyOS's musl posix_spawn is a
// vfork-style clone the Linux ABI layer refuses (docs/webkit-port-plan.md W4).
// Between fork and exec the child only calls async-signal-safe functions on
// memory prepared before the fork.
//
// A pid must never be signalled after its child was reaped (the number can belong to another
// process by then). Every child lives in `liveChildren()` from fork until its reaper thread has
// collected it, and both the collecting (waitpid with WNOHANG) and `terminateProcess`'s kill happen
// under one lock, so a kill only reaches a child that is still running or a zombie. The reaper
// blocks in `waitid(P_PID, pid, WEXITED | WNOWAIT)` (no polling, issue #887), which returns when the
// child has ended and leaves it uncollected, then takes the lock and collects it. A child it cannot
// wait for (not the case since #859) is polled until the kernel has no such id. Each child is its own
// process group and asks to be killed with its parent (PR_SET_PDEATHSIG; EINVAL is tolerated), so a
// kill of the group ends whatever it started. Every launch, exit and terminate is reported to the
// embedder (`lzw_launcher_event`, webkit/port/app/children.cpp; stderr without one, issue #862),
// queued under the lock and delivered, in order, once it is released (issue #891).

#include "config.h"
#include "ProcessLauncher.h"

#include "IPCUtilities.h"
#include <atomic>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>
#include <wtf/HashMap.h>
#include <wtf/Vector.h>
#include <wtf/Lock.h>
#include <wtf/Locker.h>
#include <wtf/NeverDestroyed.h>
#include <wtf/RunLoop.h>
#include <wtf/Threading.h>
#include <wtf/text/CString.h>
#include <wtf/unix/UnixFileDescriptor.h>

// The image's paths of the two programs, from libs/fhs (set by tools/webkit/port_cmake.py and
// WebKit/PlatformLazyOS.cmake).
#ifndef LAZYOS_WEBPROCESS_PATH
#error "LAZYOS_WEBPROCESS_PATH must name the image's webprocess (libs/fhs)"
#endif
#ifndef LAZYOS_NETWORKPROCESS_PATH
#error "LAZYOS_NETWORKPROCESS_PATH must name the image's networkprocess (libs/fhs)"
#endif

// The embedder's report (webkit/port/app/children.cpp). Weak: webkit-smoke and the other programs
// that link WebKit without it print to stderr. type: "web" or "network"; event: "launch", "fail"
// (detail: errno), "exit" (detail: status), "signal" (detail: signal), "vanish", "terminate"
// (detail, detail2: errno of kill(-pid) and of kill(pid), 0 when sent, -1 when not tried).
extern "C" __attribute__((weak)) void lzw_launcher_event(const char* type, int pid, const char* event, int detail, int detail2);

namespace WebKit {

static const char* typeName(ProcessLauncher::ProcessType type)
{
    switch (type) {
    case ProcessLauncher::ProcessType::Network:
        return "network";
    case ProcessLauncher::ProcessType::Web:
    default:
        return "web";
    }
}

static void deliver(ProcessLauncher::ProcessType type, pid_t pid, const char* event, int detail, int detail2)
{
    if (lzw_launcher_event) {
        lzw_launcher_event(typeName(type), static_cast<int>(pid), event, detail, detail2);
        return;
    }
    fprintf(stderr, "WebKit: %s process %d: %s %d %d\n", typeName(type), static_cast<int>(pid), event, detail, detail2);
}

// Where `init` finds the programs when the embedder did not say (WKContextConfigurationSetWebProcessPath).
static const char* defaultProcessPath(ProcessLauncher::ProcessType type)
{
    switch (type) {
    case ProcessLauncher::ProcessType::Network:
        return LAZYOS_NETWORKPROCESS_PATH;
    case ProcessLauncher::ProcessType::Web:
    default:
        return LAZYOS_WEBPROCESS_PATH;
    }
}

// The children that are not reaped yet, and the lock that orders every reap against every kill. A
// launch gets a generation number: LazyOS ids are task slots and are reused at once, so a thread
// that watches "its" child must be able to tell that the id now belongs to a newer one.
struct LiveChild {
    ProcessLauncher::ProcessType type;
    uint64_t generation;
};
static Lock s_childrenLock;
static HashMap<pid_t, LiveChild>& liveChildren()
{
    static NeverDestroyed<HashMap<pid_t, LiveChild>> children;
    return children;
}
static uint64_t s_nextGeneration;
// The generation of the child each launcher started (under s_childrenLock): a launcher may signal only
// the child it started, never a newer one that took the same id.
static HashMap<const ProcessLauncher*, uint64_t>& launchGenerations()
{
    static NeverDestroyed<HashMap<const ProcessLauncher*, uint64_t>> generations;
    return generations;
}

// The reports, queued under `s_childrenLock` in the order the lock saw the events and delivered after
// it is released (issue #891): the embedder prints a line for each, and a slow console must not hold
// the lock every reap and kill takes. Order matters: the embedder's list of processes follows the
// reports, and an id is reused at once (an exit reported after the next launch of the same id would
// drop the new process). So one thread at a time delivers, oldest first, and a thread that finds
// another delivering leaves its reports to it.
struct Report {
    ProcessLauncher::ProcessType type;
    pid_t pid;
    const char* event;
    int detail;
    int detail2;
};
static Vector<Report>& pendingReports()
{
    static NeverDestroyed<Vector<Report>> reports;
    return reports;
}
static Lock s_deliverLock;

// Queues a report; the caller holds `s_childrenLock` and calls `deliverReports()` after releasing it.
static void reportLocked(ProcessLauncher::ProcessType type, pid_t pid, const char* event, int detail = 0, int detail2 = 0)
{
    pendingReports().append({ type, pid, event, detail, detail2 });
}

static void deliverReports()
{
    for (;;) {
        if (!s_deliverLock.tryLock())
            return; // the thread delivering takes ours too: it looks again after it lets go
        Vector<Report> batch;
        {
            Locker locker { s_childrenLock };
            batch = std::exchange(pendingReports(), { });
        }
        for (auto& report : batch)
            deliver(report.type, report.pid, report.event, report.detail, report.detail2);
        s_deliverLock.unlock();
        Locker locker { s_childrenLock };
        if (pendingReports().isEmpty())
            return;
    }
}

static void report(ProcessLauncher::ProcessType type, pid_t pid, const char* event, int detail = 0, int detail2 = 0)
{
    {
        Locker locker { s_childrenLock };
        reportLocked(type, pid, event, detail, detail2);
    }
    deliverReports();
}

// How often a reaper that cannot wait for its child looks at it, in microseconds: the fallback only
// (a kernel without waitid's WNOWAIT, or a child this process cannot wait for).
static constexpr useconds_t reapPollInterval = 50 * 1000;

#ifndef WNOWAIT
#define WNOWAIT 0x01000000
#endif

// Times a reaper thread came back from its wait: one per child that ends (a poll adds twenty a
// second). The heartbeat prints it as `INFO:REAPER` (issue #887).
static std::atomic<unsigned long> s_reaperWakeups { 0 };

extern "C" unsigned long lzw_launcher_reaper_wakeups()
{
    return s_reaperWakeups.load(std::memory_order_relaxed);
}

// Blocks until `pid` has ended, without collecting it. Also returns when this thread cannot wait for
// the child (a kernel that refuses WNOWAIT, or one someone else collected): the caller looks at
// `liveChildren()` and collects or polls. Called with no lock held.
static void waitUntilEnded(pid_t pid)
{
    // EINTR retries are bounded: a signal that stays pending must not turn the wait into a spin.
    for (int tries = 0; tries < 100; ++tries) {
        siginfo_t info;
        memset(&info, 0, sizeof info);
        if (!waitid(P_PID, static_cast<id_t>(pid), &info, WEXITED | WNOWAIT) || errno != EINTR)
            return;
    }
}

// Collects `pid` if it is over, and takes it off the list: the one place a child is collected
// (the reaper thread and `lzw_launcher_reap`), always under `s_childrenLock`, so no kill
// can name an id that was collected (and may be somebody else's now) and no report names the wrong
// process. Returns whether the child is gone. A child this thread cannot wait for (until the
// kernel makes children per process, issue #859, a child belongs to the thread that forked it) is
// over when the kernel has no such id.
static bool collectLocked(pid_t pid, LiveChild child)
{
    int status = 0;
    const pid_t result = waitpid(pid, &status, WNOHANG);
    const int error = result == -1 ? errno : 0;
    if (result == pid) {
        liveChildren().remove(pid);
        if (WIFSIGNALED(status))
            reportLocked(child.type, pid, "signal", WTERMSIG(status));
        else
            reportLocked(child.type, pid, "exit", WEXITSTATUS(status));
        return true;
    }
    if (result == -1 && error == ECHILD) {
        if (kill(pid, 0) == -1 && errno == ESRCH) {
            liveChildren().remove(pid);
            reportLocked(child.type, pid, "vanish");
            return true;
        }
        return false;
    }
    if (result == -1 && error != EINTR) {
        liveChildren().remove(pid); // not a child of this process at all: nothing to signal or wait for
        reportLocked(child.type, pid, "vanish");
        return true;
    }
    return false;
}

// Collects every child that is over (webkit/port/app/children.cpp calls it when WebKit says a
// process ended).
extern "C" void lzw_launcher_reap()
{
    {
        Locker locker { s_childrenLock };
        Vector<std::pair<pid_t, LiveChild>> children;
        for (auto& entry : liveChildren())
            children.append({ entry.key, entry.value });
        for (auto& entry : children)
            collectLocked(entry.first, entry.second);
    }
    deliverReports();
}

// Signals every live child under the launcher's lock (the shutdown's SIGTERM and SIGKILL: a signal may
// only name an id that is still the child's, never one that was collected and reused). With `group` the
// child's process group first, the process itself if there is no such group. Fills `pids` and `codes`
// (0: sent, else the errno negated) for up to `capacity` children, and returns how many are live.
extern "C" int lzw_launcher_signal(int signal, int group, int* pids, int* codes, int capacity)
{
    Locker locker { s_childrenLock };
    int count = 0;
    for (auto& entry : liveChildren()) {
        const pid_t pid = entry.key;
        int code = 0;
        if (group && kill(-pid, signal) == 0)
            code = 0;
        else
            code = kill(pid, signal) == 0 ? 0 : -errno;
        if (count < capacity) {
            pids[count] = static_cast<int>(pid);
            codes[count] = code;
        }
        ++count;
    }
    return count;
}

// Nothing waits for an auxiliary process otherwise: the UI process learns that one is gone from
// its IPC connection, and an unreaped child stays a zombie. One thread per child, asleep in the kernel
// until its child ends (no wakeups meanwhile, issue #887); it ends when its child is collected, by
// it or by anyone else, or when the id belongs to a newer launch.
static void reapInBackground(pid_t pid, uint64_t generation)
{
    Thread::create("ProcessLauncher reaper"_s, [pid, generation] {
        // Whether the child is still ours to look at: it is in the list, and still the launch this
        // thread was started for (a collected id is reused at once by the next launch).
        auto stillOurs = [&] {
            auto found = liveChildren().find(pid);
            return found != liveChildren().end() && found->value.generation == generation;
        };
        for (;;) {
            {
                Locker locker { s_childrenLock };
                if (!stillOurs())
                    return;
            }
            waitUntilEnded(pid); // no lock: this is the long wait
            s_reaperWakeups.fetch_add(1, std::memory_order_relaxed);
            bool collected = false;
            {
                Locker locker { s_childrenLock };
                if (!stillOurs())
                    return;
                collected = collectLocked(pid, LiveChild { liveChildren().find(pid)->value });
            }
            if (collected) {
                deliverReports();
                return;
            }
            // Still not collectable after the wait returned: the fallback, polled, not spun on.
            usleep(reapPollInterval);
        }
    })->detach();
}

// The connection's two ends, both close-on-exec from the moment they exist (issue #889): only the child
// between fork and exec clears the flag on the client end. Another thread may start a program at any
// time (the ad blocker runs `curl`); an inheritable client end gave it a live end of the engine's
// connection. Returns false, errno set, when no pair can be made.
static bool makeConnectionPair(int& clientFd, UnixFileDescriptor& server)
{
    int ends[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, ends) == -1)
        return false;
    clientFd = ends[1];
    server = UnixFileDescriptor { ends[0], UnixFileDescriptor::Adopt };
    return true;
}

void ProcessLauncher::launchProcess()
{
    int clientFd = -1;
    UnixFileDescriptor serverFd;
    if (!makeConnectionPair(clientFd, serverFd)) {
        const int error = errno;
        fprintf(stderr, "WebKit: no IPC socket pair for the auxiliary process: %s\n", strerror(error));
        report(m_launchOptions.processType, 0, "fail", error);
        // As for a failed fork: an invalid identifier ends the launch, the proxy does not wait for it.
        RunLoop::mainSingleton().dispatch([protectedThis = Ref { *this }] {
            protectedThis->didFinishLaunchingProcess(0, IPC::Connection::Identifier { });
        });
        return;
    }

    CString path = !m_launchOptions.processPath.isEmpty()
        ? m_launchOptions.processPath.utf8()
        : CString(defaultProcessPath(m_launchOptions.processType));

    char processIdentifier[24];
    snprintf(processIdentifier, sizeof processIdentifier, "%llu", static_cast<unsigned long long>(m_launchOptions.processIdentifier.toUInt64()));
    char connection[16];
    snprintf(connection, sizeof connection, "%d", clientFd);

    char* argv[] = { const_cast<char*>(path.data()), processIdentifier, connection, nullptr };

    // The children start in the user's profile directory (the UI process names it in
    // LAZYOS_WEBKIT_PROFILE, engine.cpp), not in whatever directory `init` started the app in:
    // the NetworkProcess's relative paths then land in the one place its rules let it write. The
    // environment is inherited (execv), and carries the same name and the cookie jar's path.
    const char* profile = getenv("LAZYOS_WEBKIT_PROFILE");
    if (profile && *profile != '/')
        profile = nullptr;

    pid_t pid = fork();
    if (pid < 0) {
        const int error = errno;
        fprintf(stderr, "WebKit: fork failed for %s: %s\n", path.data(), strerror(error));
        report(m_launchOptions.processType, 0, "fail", error);
        close(clientFd);
        // Finish the launch without a connection (an invalid identifier, as the other launchers do
        // when they cannot start the process): the proxy stops waiting for a launch that never was.
        RunLoop::mainSingleton().dispatch([protectedThis = Ref { *this }] {
            protectedThis->didFinishLaunchingProcess(0, IPC::Connection::Identifier { });
        });
        return;
    }
    if (!pid) {
        // The one place the client end becomes inheritable: this child, about to exec (fcntl is
        // async-signal-safe). A failure leaves the flag set: the engine then finds no connection and exits.
        fcntl(clientFd, F_SETFD, 0);
        // Its own process group (a kill of the group ends whatever it started), and a SIGKILL when
        // this process dies: the kernel refuses the second with EINVAL until issue #859, which is
        // tolerated. Both are plain system calls, safe between fork and exec.
        setpgid(0, 0);
        prctl(PR_SET_PDEATHSIG, SIGKILL);
        // chdir is async-signal-safe; a profile that cannot be entered leaves the directory as it is.
        if (profile) {
            int ignored = chdir(profile);
            (void)ignored;
        }
        execv(path.data(), argv);
        // exec failed: say so with write(), which is async-signal-safe (hence the hand-made number),
        // and leave without atexit handlers.
        char message[64] = "WebKit: exec of the auxiliary process failed, errno ";
        size_t length = strlen(message);
        char digits[8];
        int count = 0;
        for (int code = errno; code > 0 && count < 8; code /= 10)
            digits[count++] = static_cast<char>('0' + code % 10);
        if (!count)
            digits[count++] = '0';
        while (count > 0)
            message[length++] = digits[--count];
        message[length++] = '\n';
        ssize_t ignored = write(STDERR_FILENO, message, length);
        (void)ignored;
        _exit(127);
    }

    m_processID = pid;
    // The parent's half of the group change as well: a kill of the group may come before the child
    // has run (the usual pair of calls; it fails harmlessly once the child has exec'd).
    setpgid(pid, pid);
    uint64_t generation;
    {
        // Entered before the reaper exists, so the reaper (and a later kill) always find it. Not
        // taken around the fork: the child must not touch a lock that another thread may hold. An
        // entry of the same id that is still here is a child nobody noticed ending: it is replaced.
        Locker locker { s_childrenLock };
        generation = ++s_nextGeneration;
        liveChildren().set(pid, LiveChild { m_launchOptions.processType, generation });
        launchGenerations().set(this, generation);
        // Queued with the entry: the launch is reported before anything the reaper finds.
        reportLocked(m_launchOptions.processType, pid, "launch");
    }
    deliverReports();
    reapInBackground(pid, generation);
    close(clientFd); // the child holds its end now

    // We've finished launching the process, message back to the main run loop.
    RunLoop::mainSingleton().dispatch([protectedThis = Ref { *this }, pid, serverIdentifier = WTF::move(serverFd)] mutable {
        protectedThis->didFinishLaunchingProcess(pid, IPC::Connection::Identifier { WTF::move(serverIdentifier) });
    });
}

void ProcessLauncher::terminateProcess()
{
    if (m_isLaunching) {
        invalidate();
        return;
    }

    if (!m_processID)
        return;

    {
        // Only a child the reaper has not collected: a collected pid may be someone else's now.
        // The group first (the child made itself its leader), then the process itself if there was
        // no such group; what each said is reported (issue #862: a terminate that sent nothing
        // left a busy NetworkProcess running beside its replacement).
        Locker locker { s_childrenLock };
        auto found = liveChildren().find(m_processID);
        const uint64_t mine = launchGenerations().take(this);
        if (found != liveChildren().end() && mine && found->value.generation == mine) {
            const int groupError = kill(-m_processID, SIGKILL) == 0 ? 0 : errno;
            int pidError = -1;
            if (groupError)
                pidError = kill(m_processID, SIGKILL) == 0 ? 0 : errno;
            reportLocked(m_launchOptions.processType, m_processID, "terminate", groupError, pidError);
        } else
            reportLocked(m_launchOptions.processType, m_processID, "terminate", -1, -1);
    }
    // Printed with the lock released: WebKit's terminate() does not wait for a slow console.
    deliverReports();
    m_processID = 0;
}

void ProcessLauncher::platformInvalidate()
{
}

} // namespace WebKit
