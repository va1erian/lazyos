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
 * MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
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

#include "config.h"
#include "ProcessLauncher.h"

#include "IPCUtilities.h"
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>
#include <wtf/RunLoop.h>
#include <wtf/Threading.h>
#include <wtf/text/CString.h>

namespace WebKit {

// Where `init` finds the programs when the embedder did not say (WKContextConfigurationSetWebProcessPath).
static const char* defaultProcessPath(ProcessLauncher::ProcessType type)
{
    switch (type) {
    case ProcessLauncher::ProcessType::Network:
        return "/system/bin/NetworkProcess";
    case ProcessLauncher::ProcessType::Web:
    default:
        return "/system/bin/WebProcess";
    }
}

// Nothing waits for an auxiliary process otherwise: the UI process learns that one is gone from
// its IPC connection, and an unreaped child stays a zombie. One short-lived thread per child.
static void reapInBackground(pid_t pid)
{
    Thread::create("ProcessLauncher reaper"_s, [pid] {
        int status = 0;
        while (waitpid(pid, &status, 0) == -1 && errno == EINTR) { }
        if (WIFSIGNALED(status))
            fprintf(stderr, "WebKit: auxiliary process %d killed by signal %d\n", static_cast<int>(pid), WTERMSIG(status));
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

    pid_t pid = fork();
    if (pid < 0) {
        fprintf(stderr, "WebKit: fork failed for %s: %s\n", path.data(), strerror(errno));
        return;
    }
    if (!pid) {
        execv(path.data(), argv);
        // exec failed: say so with write(), which is async-signal-safe, and leave without atexit handlers.
        static const char message[] = "WebKit: exec of the auxiliary process failed\n";
        ssize_t ignored = write(STDERR_FILENO, message, sizeof message - 1);
        (void)ignored;
        _exit(127);
    }

    m_processID = pid;
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

    kill(m_processID, SIGKILL);
    m_processID = 0;
}

void ProcessLauncher::platformInvalidate()
{
}

} // namespace WebKit
