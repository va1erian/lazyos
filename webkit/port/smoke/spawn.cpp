// webkit-smoke's spawn probe (issue #889, spawn.h).
//
// The launcher (ProcessLauncherLazyOS.cpp) makes a SOCK_SEQPACKET socket pair for each engine process,
// forks, and gives the child its end across the exec. Any other thread of the UI process that starts a
// program while that is going on (LazyWeb's ad blocker runs `curl` from a worker thread) used to
// receive the client end too: it was inheritable until the parent closed it after the fork, and the
// parent only did so after reports and the reaper thread's start. Both ends are close-on-exec now and
// only the forked child clears the flag.
//
// The launcher tells its embedder about every launch (`lzw_launcher_event`, weak) at exactly that
// point: after the fork, with the client end still open in the parent. This program is the embedder:
// at each "launch" it starts a copy of itself in probe mode (/proc/self/exe --fd-probe), which
// lists the sockets it was handed (fstat: LazyOS answers no getsockopt on a socketpair end), and
// waits for it. The probe is first run against a socket pair left inheritable on purpose (the
// control), which it must report: a detector that sees nothing would pass the real check too. It runs on the launching
// thread, which does not matter: a descriptor is inherited by any program the process starts
// while it is open, whichever thread starts it.
//
//   WEBKIT:W4:SPAWN:CONTROL:PASS              (the probe saw the deliberate leak)
//   WEBKIT:W4:SPAWN:PASS:launches=<n>
//   WEBKIT:W4:SPAWN:LEAK:fd=<n>               (from the probe, on stdout)
//   WEBKIT:W4:FAIL:spawn:<reason>             (exit status 1)

#include "spawn.h"

#include <atomic>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

namespace smoke {
namespace {

constexpr const char* kProbeArgument = "--fd-probe";
// Never look beyond this descriptor: the descriptor mode holds about a thousand and the IPC sockets
// follow them, the kernel's per-task ceiling is 8192.
constexpr rlim_t kScanLimit = 8192;

std::atomic<int> s_launches { 0 };

[[noreturn]] void fail(const char* reason)
{
    printf("WEBKIT:W4:FAIL:spawn:%s\n", reason);
    fflush(stdout);
    _exit(1);
}

} // namespace

// Runs the probe program and waits for it; its exit status: 0 nothing inherited, 1 sockets found.
int runProbe(bool quiet)
{
    const pid_t pid = fork();
    if (pid < 0)
        fail("fork of the probe failed");
    if (!pid) {
        // The control's probe says nothing about the sockets it finds: the judge reads LEAK lines as failures.
        char* argv[] = { const_cast<char*>("webkit-smoke"), const_cast<char*>(kProbeArgument), const_cast<char*>(quiet ? "quiet" : ""), nullptr };
        execv("/proc/self/exe", argv);
        _exit(127);
    }
    int status = 0;
    pid_t result;
    do {
        result = waitpid(pid, &status, 0);
    } while (result == -1 && errno == EINTR);
    if (result != pid)
        fail("waitpid of the probe failed");
    if (!WIFEXITED(status) || WEXITSTATUS(status) > 1)
        fail("the probe did not run (exec of /proc/self/exe failed or it was killed)");
    return WEXITSTATUS(status);
}

// At a launch: the program started now must not have been lent the engine's connection.
void runProbeAtLaunch()
{
    ++s_launches;
    if (runProbe(false))
        fail("a program started while an engine process was launching inherited an IPC socket");
}

void runControl()
{
    int ends[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, ends) == -1) // inheritable on purpose
        fail("no socket pair for the control");
    const int saw = runProbe(true);
    close(ends[0]);
    close(ends[1]);
    if (!saw)
        fail("the probe did not see a socket it was handed: it cannot detect a leak");
    printf("WEBKIT:W4:SPAWN:CONTROL:PASS\n");
    fflush(stdout);
}

void runChildProbeIfAsked(int argc, char** argv)
{
    if (argc < 2 || strcmp(argv[1], kProbeArgument))
        return;
    rlim_t limit = kScanLimit;
    rlimit current;
    if (!getrlimit(RLIMIT_NOFILE, &current) && current.rlim_cur < limit)
        limit = current.rlim_cur;
    int leaks = 0;
    for (int fd = 3; fd < static_cast<int>(limit); ++fd) {
        // Every socket of this program is close-on-exec but the control's: any one here was lent.
        struct stat info;
        if (fstat(fd, &info) == 0 && S_ISSOCK(info.st_mode)) {
            if (argc < 3 || *argv[2] == 0)
                printf("WEBKIT:W4:SPAWN:LEAK:fd=%d\n", fd);
            ++leaks;
        }
    }
    fflush(stdout);
    _exit(leaks ? 1 : 0);
}

void startSpawnProbe()
{
    runControl();
}

void finishSpawnProbe()
{
    const int launches = s_launches.load();
    if (!launches)
        fail("the launcher reported no launch, so nothing was probed");
    printf("WEBKIT:W4:SPAWN:PASS:launches=%d\n", launches);
    fflush(stdout);
}

} // namespace smoke

// The launcher's report of each launch, exit and terminate (ProcessLauncherLazyOS.cpp; the browser's is
// webkit/port/app/children.cpp). At "launch" the engine's end of the connection is still open here.
// The line goes to stderr as it does without an embedder.
extern "C" void lzw_launcher_event(const char* type, int pid, const char* event, int detail, int detail2)
{
    fprintf(stderr, "WebKit: %s process %d: %s %d %d\n", type, pid, event, detail, detail2);
    if (!strcmp(event, "launch"))
        smoke::runProbeAtLaunch();
}
