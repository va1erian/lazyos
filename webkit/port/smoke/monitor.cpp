// webkit-smoke's descriptor mode (issue #867, monitor.h).
//
// WebKit's IPC connections on LazyOS are watched by a SocketMonitor thread per connection
// (ConnectionUnix.cpp, patch 0022). It used to wait with select() on an fd_set, which cannot hold a
// descriptor of 1024 or more (FD_SET wrote past it, and LazyOS's select answers EINVAL, which the loop
// retried at once). With WEBKIT_SMOKE_FDS=<n> the program holds n close-on-exec descriptors before
// WebKit starts, so the rest of the run is WebKit's IPC above 1024 in all three processes, and first
// runs the monitor's loop itself (the same header ConnectionUnix.cpp includes) on a socket pair above
// 1024, the way a connection uses it:
//
//   1. the peer sends three messages and closes: the monitor reads all three, sees the close and
//      ends, with a handful of wakeups (a spin would show thousands, or never end);
//   2. the connection lets go of its descriptor (as platformInvalidate does) while the monitor
//      waits: the monitor ends within its one-second bound.
//
//   WEBKIT:W4:FDS:held=<n>:next=<fd>
//   WEBKIT:W4:MONITOR:PASS:fd=<fd>:messages=<n>:wakeups=<n>:close_ms=<ms>:invalidate_ms=<ms>
//   WEBKIT:W4:FAIL:monitor:<reason>          (exit status 1)

#include "monitor.h"

#include "SocketMonitorLazyOS.h"

#include <atomic>
#include <chrono>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <thread>
#include <unistd.h>
#include <vector>

namespace smoke {
namespace {

constexpr int kMessages = 3;
// More than this many reads for three messages and a close is a spin.
constexpr int kMaxWakeups = 16;
constexpr long kCloseLimitMs = 5000;
constexpr long kInvalidateLimitMs = 3000;
constexpr int kFirstHighDescriptor = 1024;

[[noreturn]] void fail(const char* reason)
{
    printf("WEBKIT:W4:FAIL:monitor:%s\n", reason);
    fflush(stdout);
    _exit(1);
}

long nowMs()
{
    using namespace std::chrono;
    return duration_cast<milliseconds>(steady_clock::now().time_since_epoch()).count();
}

// The number of descriptors WEBKIT_SMOKE_FDS asks for, or 0.
int requested()
{
    const char* text = getenv("WEBKIT_SMOKE_FDS");
    if (!text || !*text)
        return 0;
    char* end = nullptr;
    const long count = strtol(text, &end, 10);
    if (!end || *end || count < 1 || count > 7000)
        fail("WEBKIT_SMOKE_FDS is not a number of descriptors from 1 to 7000");
    return static_cast<int>(count);
}

// Holds `count` close-on-exec copies of one pipe end (the engine processes do not inherit them), with
// the soft descriptor limit raised to the hard one if need be. The program never closes them.
// Returns that pipe end.
int hold(int count)
{
    rlimit limit;
    if (!getrlimit(RLIMIT_NOFILE, &limit) && limit.rlim_cur < limit.rlim_max) {
        limit.rlim_cur = limit.rlim_max;
        setrlimit(RLIMIT_NOFILE, &limit);
    }
    int ends[2];
    if (pipe2(ends, O_CLOEXEC))
        fail("pipe2 failed");
    for (int i = 2; i < count; ++i) {
        if (fcntl(ends[0], F_DUPFD_CLOEXEC, 0) < 0)
            fail("cannot hold the descriptors (F_DUPFD_CLOEXEC)");
    }
    return ends[0];
}

struct Pair {
    int monitored;
    int peer;
};

// A connection's socket pair: SOCK_SEQPACKET, and the monitored end non-blocking as WebKit's is.
Pair socketPair()
{
    int ends[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, ends))
        fail("socketpair failed");
    const int flags = fcntl(ends[0], F_GETFL);
    if (flags < 0 || fcntl(ends[0], F_SETFL, flags | O_NONBLOCK))
        fail("cannot make the socket non-blocking");
    if (ends[0] < kFirstHighDescriptor || ends[1] < kFirstHighDescriptor)
        fail("the socket pair is below descriptor 1024: hold more descriptors");
    return { ends[0], ends[1] };
}

// What a connection does around the loop: the descriptor it holds (-1 once let go of), the reads
// (readyReadHandler) and the close (connectionDidClose, which lets go of the descriptor).
struct Connection {
    std::atomic<int> descriptor;
    std::atomic<int> messages { 0 };
    std::atomic<int> wakeups { 0 };
    std::atomic<bool> closed { false };
    std::atomic<bool> ended { false };
    std::atomic<int> end { -1 };

    explicit Connection(int fd)
        : descriptor(fd)
    {
    }

    void letGo()
    {
        const int fd = descriptor.exchange(-1);
        if (fd >= 0)
            close(fd);
    }

    void readyRead()
    {
        ++wakeups;
        char buffer[256];
        for (;;) {
            const int fd = descriptor.load();
            if (fd < 0)
                return;
            const ssize_t got = recv(fd, buffer, sizeof buffer, 0);
            if (got > 0) {
                ++messages;
                continue;
            }
            if (got < 0 && errno == EINTR)
                continue;
            if (got < 0 && (errno == EAGAIN || errno == EWOULDBLOCK))
                return;
            closed = true;
            letGo();
            return;
        }
    }

    void run()
    {
        const auto result = IPC::LazyOS::monitorSocket(
            [this] { return descriptor.load(); },
            [this] { readyRead(); },
            [this] { closed = true; letGo(); });
        end = static_cast<int>(result);
        ended = true;
    }
};

// Waits up to `limitMs` for the monitor to end; the time it took, or -1.
long waitForEnd(Connection& connection, long started, long limitMs)
{
    while (!connection.ended.load()) {
        if (nowMs() - started > limitMs)
            return -1;
        usleep(2000);
    }
    return nowMs() - started;
}

// 1: the peer sends and hangs up.
long peerCloses(int& firstDescriptor, int& messages, int& wakeups)
{
    const Pair pair = socketPair();
    firstDescriptor = pair.monitored;
    Connection connection(pair.monitored);
    std::thread monitor([&] { connection.run(); });
    for (int i = 0; i < kMessages; ++i) {
        const char message[] = "lazyos ipc";
        if (send(pair.peer, message, sizeof message, MSG_NOSIGNAL) != static_cast<ssize_t>(sizeof message))
            fail("the peer cannot send");
        usleep(20000);
    }
    const long started = nowMs();
    close(pair.peer);
    const long took = waitForEnd(connection, started, kCloseLimitMs);
    if (took < 0) {
        printf("WEBKIT:W4:INFO:MONITOR:wakeups=%d:messages=%d\n", connection.wakeups.load(), connection.messages.load());
        fail("the monitor did not see the peer close within 5 s");
    }
    monitor.join();
    messages = connection.messages.load();
    wakeups = connection.wakeups.load();
    if (messages != kMessages)
        fail("the monitor did not read every message");
    if (!connection.closed.load())
        fail("the monitor ended without the connection closing");
    if (wakeups > kMaxWakeups)
        fail("the monitor woke too often (a spin)");
    return took;
}

// 2: the connection lets go of the descriptor while the monitor waits.
long connectionLetsGo()
{
    const Pair pair = socketPair();
    Connection connection(pair.monitored);
    std::thread monitor([&] { connection.run(); });
    usleep(50000);
    const long started = nowMs();
    connection.letGo();
    const long took = waitForEnd(connection, started, kInvalidateLimitMs);
    if (took < 0)
        fail("the monitor did not end within 3 s of the connection letting go of its socket");
    monitor.join();
    close(pair.peer);
    if (connection.end.load() != static_cast<int>(IPC::LazyOS::SocketMonitorEnd::Invalidated))
        fail("the monitor did not end as invalidated");
    return took;
}

} // namespace

void runDescriptorMode()
{
    const int count = requested();
    if (!count)
        return;
    const int held = hold(count);
    const int next = fcntl(held, F_DUPFD_CLOEXEC, 0);
    if (next >= 0)
        close(next);
    printf("WEBKIT:W4:FDS:held=%d:next=%d\n", count, next);
    int fd = -1, messages = 0, wakeups = 0;
    const long closeMs = peerCloses(fd, messages, wakeups);
    const long invalidateMs = connectionLetsGo();
    printf("WEBKIT:W4:MONITOR:PASS:fd=%d:messages=%d:wakeups=%d:close_ms=%ld:invalidate_ms=%ld\n", fd, messages, wakeups,
        closeMs, invalidateMs);
    fflush(stdout);
}

} // namespace smoke
