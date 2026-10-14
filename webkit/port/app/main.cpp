#ifndef _GNU_SOURCE
#define _GNU_SOURCE 1
#endif
// The desktop UI process: WebKit in a window (docs/webkit-port-plan.md, W5; docs/lazywebplus.md).
// One program for both windows: `webkit-app` links the W5 window, `lazywebplus-app` LazyWeb's
// (the Rust library each one links is the only difference).
//
// The UI process has two threads that matter: WebKit's run loop on the process's main thread, as
// in webkit-smoke (engine.cpp), and the xui window on a second one (a Rust static library through
// lazyos_ui.h). A Messenger handle belongs to the thread that made it, so the window is built, used
// and destroyed on its own thread. When the window closes the run loop ends, the UI process ends
// the WebProcess and the NetworkProcess (SIGTERM, SIGKILL after a grace period), waits for them
// (<prefix>:CHILD:<name>:<pid>:GONE) and exits.
//
// Markers, after the window's prefix (WEBKIT:W5 or WEBPLUS):
//   UP                 the UI process started and WebKit2 initialised
//   CHILD:<name>:<pid>:GONE   a child process ended (ALIVE: it did not), with the results of the
//                      SIGTERM, SIGKILL and wait that ended it (children.cpp)
//   QUIT:PASS          the window closed, the run loop ended and the children are gone
//   (and the markers of engine.cpp and the window's own)

#include <WebKit/WKRunLoop.h>

#include <pthread.h>
#include <signal.h>
#include <ucontext.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

#include "engine.h"
#include "lazyos_ui.h"

extern "C" const char __ehdr_start;

namespace {

// A crash (WebKit's CRASH is a SIGSEGV at 0xbbadbeef or an illegal instruction, an assertion or a pure
// virtual call is SIGABRT) leaves no message anywhere on this system: the kernel says only which signal
// ended the task. So the handler prints the signal, where it was (rip, rsp) and every word of the first
// 16 KiB of the stack (never past the top of this thread's stack, noted when the handler is installed)
// that points into the program's code, which a person reads with
// `addr2line -f -C -e <the unstripped program> <at>` (the program is not position independent: the words
// are addresses as they are). The first frames below rsp are the crash's own; the callers follow (some
// words are stale, as on any stack walked by scanning). It runs on its own stack (a stack overflow is
// one of the crashes), builds its lines by hand (nothing it calls may take a lock or allocate), and the
// handler is reset on entry (SA_RESETHAND): a fault inside it, past the end of the stack, ends the
// process the way it ended without the handler, and returning re-raises the original signal.
constexpr int kScanWords = 2048;
constexpr int kScanPrint = 48;
// Where the crashing thread's stack ends (a little above the frame that installed the handler; 0: not
// known, so the scan stops at the end of the page rsp is in). One word per thread of TLS, not a buffer.
thread_local unsigned long stackTop = 0;
constexpr unsigned long kStackTopMargin = 256;

char* putText(char* at, const char* text)
{
    while (*text)
        *at++ = *text++;
    return at;
}

char* putHex(char* at, unsigned long value)
{
    char digits[16];
    int count = 0;
    do {
        digits[count++] = "0123456789abcdef"[value & 15];
        value >>= 4;
    } while (value && count < 16);
    while (count)
        *at++ = digits[--count];
    return at;
}

// "<prefix>:CRASH:<name>=<a>[<second><b>[<third><c>]]\n", in one write.
void putLine(const char* name, unsigned long a, const char* second = nullptr, unsigned long b = 0, const char* third = nullptr, unsigned long c = 0)
{
    char line[200];
    char* at = putText(line, lzw_ui_marker_prefix());
    at = putText(at, ":CRASH:");
    at = putText(at, name);
    at = putText(at, "=");
    at = putHex(at, a);
    if (second) {
        at = putText(at, second);
        at = putHex(at, b);
    }
    if (third) {
        at = putText(at, third);
        at = putHex(at, c);
    }
    *at++ = '\n';
    ssize_t ignored = write(STDOUT_FILENO, line, static_cast<size_t>(at - line));
    (void)ignored;
}

void reportCrash(int signal, siginfo_t* info, void* context)
{
    // musl's abort() ends with a trap of its own after the handler returns: that second crash is not news.
    if (signal == SIGABRT) {
        for (int number : { SIGSEGV, SIGILL, SIGTRAP, SIGBUS, SIGFPE })
            ::signal(number, SIG_DFL);
    }
    const unsigned long base = reinterpret_cast<unsigned long>(&__ehdr_start);
    auto* uc = static_cast<ucontext_t*>(context);
    const unsigned long rsp = static_cast<unsigned long>(uc->uc_mcontext.gregs[REG_RSP]);
    const unsigned long rip = static_cast<unsigned long>(uc->uc_mcontext.gregs[REG_RIP]);
    putLine("signal", static_cast<unsigned long>(signal), ":base=", base, ":rip=", rip);
    putLine("rsp", rsp, ":addr=", reinterpret_cast<unsigned long>(info ? info->si_addr : nullptr), ":top=", stackTop);
    const unsigned long* words = reinterpret_cast<const unsigned long*>(rsp);
    // Up to the top of the stack the thread noted when it installed the handler (the main and window
    // threads); any other thread (WebKit's workers, the reapers) to the end of rsp's page, which is
    // mapped whenever rsp is. A top that does not lie above rsp (an overflow, a corrupt frame) is not
    // trusted either.
    unsigned long available = 0;
    if (stackTop > rsp && (stackTop - rsp) <= (64UL << 20))
        available = (stackTop - rsp) / sizeof(unsigned long);
    else
        available = ((rsp | 4095UL) + 1 - rsp) / sizeof(unsigned long);
    const int limit = available < static_cast<unsigned long>(kScanWords) ? static_cast<int>(available) : kScanWords;
    int printed = 0;
    for (int i = 0; i < limit && printed < kScanPrint; ++i) {
        const unsigned long word = words[i];
        if (word > base && word < base + 0x8000000UL) {
            putLine("at", word);
            ++printed;
        }
    }
    // The handler is reset (SA_RESETHAND) and SIGABRT is blocked while it runs: unblock it and raise it
    // again for a SIGABRT (so the exit status is the signal's, as without the handler), `_exit(134)`
    // (128 + SIGABRT) if the system does not deliver it. Other signals re-fault on return.
    if (signal == SIGABRT) {
        sigset_t pending;
        sigemptyset(&pending);
        sigaddset(&pending, SIGABRT);
        sigprocmask(SIG_UNBLOCK, &pending, nullptr);
        raise(SIGABRT);
        _exit(134);
    }
}

// The stack the handler runs on, for the thread that asks: its own array, one for the main thread and
// one for the window's. (Not `thread_local`: that would add its size to the TLS block of every
// thread the process ever starts, and musl keeps a thread's TLS in its stack mapping: a 128 KiB
// thread stack of WebKit's would lose half of it.)
char mainCrashStack[64 * 1024];
char windowCrashStack[64 * 1024];

void useCrashStack(char* stack, size_t size)
{
    stack_t alternate = { };
    alternate.ss_sp = stack;
    alternate.ss_size = size;
    sigaltstack(&alternate, nullptr);
}

void installCrashHandler(const void* frame)
{
    stackTop = reinterpret_cast<unsigned long>(frame) + kStackTopMargin;
    useCrashStack(mainCrashStack, sizeof mainCrashStack);
    struct sigaction action = { };
    action.sa_sigaction = reportCrash;
    action.sa_flags = SA_SIGINFO | SA_ONSTACK | SA_RESETHAND;
    for (int number : { SIGSEGV, SIGILL, SIGTRAP, SIGBUS, SIGFPE, SIGABRT })
        sigaction(number, &action, nullptr);
}

struct Arguments {
    int argc;
    char** argv;
    int status;
};

void* windowThread(void* context)
{
    auto* arguments = static_cast<Arguments*>(context);
    useCrashStack(windowCrashStack, sizeof windowCrashStack);
    stackTop = reinterpret_cast<unsigned long>(__builtin_frame_address(0)) + kStackTopMargin;
    arguments->status = lzw_ui_main(arguments->argc, arguments->argv);
    lzw_engine_quit();
    return nullptr;
}

} // namespace

int main(int argc, char** argv)
{
    setvbuf(stdout, nullptr, _IOLBF, 0);
    installCrashHandler(__builtin_frame_address(0));
    // A WebProcess or NetworkProcess that dies (a crash, the OOM kill, a refused call it did not
    // survive) closes its end of the IPC socket; the next `write` here must be `EPIPE`, not a
    // SIGPIPE that ends the window with it. (WebKit sends with MSG_NOSIGNAL; the Rust half and any
    // plain write do not.) A C++ `main` does not get the Rust runtime's ignore of it.
    signal(SIGPIPE, SIG_IGN);
    WKRunLoopInitializeMain();
    webkitapp::say("UP");
    if (lzw_engine_start() != 0)
        return 1;

    static Arguments arguments { argc, argv, 0 };
    pthread_attr_t attributes;
    pthread_attr_init(&attributes);
    // xui lays out and shapes text on this stack: the 128 KiB default of a musl thread is too small.
    pthread_attr_setstacksize(&attributes, 8u << 20);
    pthread_t window;
    if (pthread_create(&window, &attributes, windowThread, &arguments) != 0) {
        webkitapp::say("FAIL:window:cannot start the window's thread");
        return 1;
    }
    pthread_detach(window);

    // The first thing the loop runs: from here on it is turning (the window's heartbeat times it).
    WKRunLoopCallOnMainThread([](void*) { webkitapp::E.running = true; }, nullptr);
    WKRunLoopRunMain();
    // End the WebProcess and the NetworkProcess ourselves and wait for them: the sockets closing
    // is not something to rely on, and a process left behind holds its 107 MB and its share of the
    // user's memory quota.
    const int surviving = lzw_engine_shutdown();
    webkitapp::say("QUIT:%s", arguments.status == 0 && surviving == 0 ? "PASS" : "FAIL");
    fflush(stdout);
    // Non-zero when the window failed or an engine process is left behind (3): the person who launched
    // this, and the harness, can tell.
    _exit(arguments.status != 0 ? arguments.status : (surviving != 0 ? 3 : 0));
}
