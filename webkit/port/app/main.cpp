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

// An abort (WebKit's CRASH, an assertion, a pure virtual call) leaves no message anywhere on this
// system, and the kernel says only that signal 6 killed the task. So the handler prints where it
// was (rip, rsp) and every word of the stack's first 32 KiB that points into the program's code,
// which a person reads with `addr2line -f -C -e <the unstripped program> <at>` (the program is not
// position independent: the words are addresses as they are). The first frames below rsp are the
// abort's own; the callers follow (some words are stale, as on any unwound-by-scanning stack).
void reportAbort(int, siginfo_t*, void* context)
{
    char line[256];
    const unsigned long base = reinterpret_cast<unsigned long>(&__ehdr_start);
    auto* uc = static_cast<ucontext_t*>(context);
    const unsigned long rsp = static_cast<unsigned long>(uc->uc_mcontext.gregs[15]);
    const unsigned long rip = static_cast<unsigned long>(uc->uc_mcontext.gregs[16]);
    int n = snprintf(line, sizeof line, "%s:ABORT:base=%lx:rip=%lx:rsp=%lx\n", lzw_ui_marker_prefix(), base, rip, rsp);
    write(STDOUT_FILENO, line, n);
    const unsigned long* words = reinterpret_cast<const unsigned long*>(rsp);
    int printed = 0;
    for (int i = 0; i < 4096 && printed < 48; ++i) {
        const unsigned long word = words[i];
        if (word > base && word < base + 0x8000000UL) {
            n = snprintf(line, sizeof line, "%s:ABORT:at=%lx\n", lzw_ui_marker_prefix(), word);
            write(STDOUT_FILENO, line, n);
            ++printed;
        }
    }
    _exit(134);
}

struct Arguments {
    int argc;
    char** argv;
    int status;
};

void* windowThread(void* context)
{
    auto* arguments = static_cast<Arguments*>(context);
    arguments->status = lzw_ui_main(arguments->argc, arguments->argv);
    lzw_engine_quit();
    return nullptr;
}

} // namespace

int main(int argc, char** argv)
{
    setvbuf(stdout, nullptr, _IOLBF, 0);
    struct sigaction action = { };
    action.sa_sigaction = reportAbort;
    action.sa_flags = SA_SIGINFO;
    sigaction(SIGABRT, &action, nullptr);
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
