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

// The loop of an IPC connection's SocketMonitor thread on LazyOS (issue #867;
// ConnectionUnix.cpp, patch 0022). Plain POSIX and header-only, so webkit-smoke
// (webkit/port/smoke/monitor.cpp) runs this very code against a socket above
// descriptor 1024 on the build host and in the guest.
//
// The PlayStation loop it replaces waited with select() on a stack fd_set: with a
// descriptor of 1024 or more FD_SET wrote past the set, and LazyOS refuses
// nfds > 1024 with EINVAL, which the loop retried at once, a spin that never saw
// the connection close. poll() has no such limit. A hangup or an error that the
// read leaves open is a close too: polling again would only report it again.

#pragma once

#include <errno.h>
#include <poll.h>

namespace IPC {
namespace LazyOS {

// The longest one wait lasts. A close of the descriptor wakes a poll on LazyOS, not
// on Linux; the bound makes an invalidated connection's monitor end there as well.
constexpr int socketMonitorWaitMilliseconds = 1000;

enum class SocketMonitorEnd {
    Invalidated, // the connection let go of the descriptor (or holds another one)
    Closed, // the peer hung up, or the descriptor failed: `closed` was called
    Failed, // poll() itself failed: `closed` was called
};

// Watches the descriptor `descriptor()` names until the connection lets go of it
// (-1, or a different number), calling `readyRead()` when it can be read and
// `closed()` when it never can again and the read did not say so itself.
template<typename Descriptor, typename ReadyRead, typename Closed>
SocketMonitorEnd monitorSocket(const Descriptor& descriptor, const ReadyRead& readyRead, const Closed& closed)
{
    for (;;) {
        const int fd = descriptor();
        if (fd < 0)
            return SocketMonitorEnd::Invalidated;
        struct pollfd entry = { fd, POLLIN, 0 };
        const int ready = poll(&entry, 1, socketMonitorWaitMilliseconds);
        const int error = errno;
        // Let go of while this thread waited: the number may be another file's by now.
        if (descriptor() != fd)
            return SocketMonitorEnd::Invalidated;
        if (ready < 0) {
            if (error == EINTR || error == EAGAIN)
                continue;
            closed();
            return SocketMonitorEnd::Failed;
        }
        if (!ready)
            continue;
        if (entry.revents & (POLLIN | POLLHUP | POLLERR))
            readyRead();
        if ((entry.revents & (POLLHUP | POLLERR | POLLNVAL)) && descriptor() == fd) {
            closed();
            return SocketMonitorEnd::Closed;
        }
    }
}

} // namespace LazyOS
} // namespace IPC
