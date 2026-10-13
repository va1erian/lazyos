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
// WebProcess or NetworkProcess with the client end left open across the exec
// (the server end is close-on-exec), the command line the generic unix
// AuxiliaryProcessMain expects: "<program> <process identifier> <client fd>".
//
// fork and execv rather than posix_spawn: LazyOS's musl posix_spawn is a
// vfork-style clone the Linux ABI layer refuses (docs/webkit-port-plan.md W4).
// Between fork and exec the child only calls async-signal-safe functions on
// memory prepared before the fork.
//
// A pid must never be signalled after its child was reaped (the number can belong to another
// process by then). Every child lives in `liveChildren()` from fork until its reaper thread
// has collected it, and both the collecting (waitpid with WNOHANG, polled: LazyOS has no
// waitid with WNOWAIT to wait without reaping) and `terminateProcess`'s kill happen under one
// lock, so a kill only ever reaches a child that is still running or a zombie.

#include "config.h"
#include "ProcessLauncher.h"

#include "IPCUtilities.h"
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>
#include <wtf/HashSet.h>
#include <wtf/Lock.h>
#include <wtf/Locker.h>
#include <wtf/NeverDestroyed.h>
#include <wtf/RunLoop.h>
#include <wtf/Threading.h>
#include <wtf/text/CString.h>

// The image's paths of the two programs, from libs/fhs (set by tools/webkit/port_cmake.py and
// WebKit/PlatformLazyOS.cmake).
#ifndef LAZYOS_WEBPROCESS_PATH
#error "LAZYOS_WEBPROCESS_PATH must name the image's webprocess (libs/fhs)"
#endif
#ifndef LAZYOS_NETWORKPROCESS_PATH
#error "LAZYOS_NETWORKPROCESS_PATH must name the image's networkprocess (libs/fhs)"
#endif

namespace WebKit {

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

// The children that are not reaped yet, and the lock that orders a reap against a kill.
static Lock s_childrenLock;
static HashSet<pid_t>& liveChildren()
{
    static NeverDestroyed<HashSet<pid_t>> children;
    return children;
}

// How often the reaper looks at its child, in microseconds: a finished child is a zombie for
// at most this long.
static constexpr useconds_t reapPollInterval = 50 * 1000;

// Nothing waits for an auxiliary process otherwise: the UI process learns that one is gone from
// its IPC connection, and an unreaped child stays a zombie. One short-lived thread per child.
static void reapInBackground(pid_t pid)
{
    Thread::create("ProcessLauncher reaper"_s, [pid] {
        for (;;) {
            int status = 0;
            pid_t result;
            {
                Locker locker { s_childrenLock };
                result = waitpid(pid, &status, WNOHANG);
                if (result == pid || (result == -1 && errno != EINTR))
                    liveChildren().remove(pid); // reaped (or not ours any more): no kill may name it
            }
            if (result == pid) {
                if (WIFSIGNALED(status))
                    fprintf(stderr, "WebKit: auxiliary process %d killed by signal %d\n", static_cast<int>(pid), WTERMSIG(status));
                return;
            }
            if (result == -1 && errno != EINTR)
                return;
            usleep(reapPollInterval);
        }
    })->detach();
}

void ProcessLauncher::launchProcess()
{
    // The client end stays open across exec; only the server end is close-on-exec.
    IPC::SocketPair socketPair = IPC::createPlatformConnection(SOCK_SEQPACKET, IPC::PlatformConnectionOptions::SetCloexecOnServer);

    CString path = !m_launchOptions.processPath.isEmpty()
        ? m_launchOptions.processPath.utf8()
        : CString(defaultProcessPath(m_launchOptions.processType));

    char processIdentifier[24];
    snprintf(processIdentifier, sizeof processIdentifier, "%llu", static_cast<unsigned long long>(m_launchOptions.processIdentifier.toUInt64()));
    char connection[16];
    snprintf(connection, sizeof connection, "%d", socketPair.client.value());

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
        fprintf(stderr, "WebKit: fork failed for %s: %s\n", path.data(), strerror(errno));
        // Finish the launch without a connection (an invalid identifier, as the other launchers do
        // when they cannot start the process): the proxy stops waiting for a launch that never was.
        RunLoop::mainSingleton().dispatch([protectedThis = Ref { *this }] {
            protectedThis->didFinishLaunchingProcess(0, IPC::Connection::Identifier { });
        });
        return;
    }
    if (!pid) {
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
    {
        // Entered before the reaper exists, so the reaper (and a later kill) always find it. Not
        // taken around the fork: the child must not touch a lock that another thread may hold.
        Locker locker { s_childrenLock };
        liveChildren().add(pid);
    }
    reapInBackground(pid);
    close(socketPair.client.release()); // the child holds its end now

    // We've finished launching the process, message back to the main run loop.
    RunLoop::mainSingleton().dispatch([protectedThis = Ref { *this }, pid, serverIdentifier = WTF::move(socketPair.server)] mutable {
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
        Locker locker { s_childrenLock };
        if (liveChildren().contains(m_processID))
            kill(m_processID, SIGKILL);
    }
    m_processID = 0;
}

void ProcessLauncher::platformInvalidate()
{
}

} // namespace WebKit
