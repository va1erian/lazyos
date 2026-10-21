#ifndef _GNU_SOURCE
#define _GNU_SOURCE 1
#endif
// webd (docs/webd.md, docs/webd-plan.md P2 slice 2): the program that owns WebKit for the session and serves
// light views over Messenger. `webd-app` links the engine (webkit/port/app), its C ABI for the service
// (abi.cpp, webd_abi.h) and the service itself, a Rust static library (xui-app/webd, host.h).
//
// Two threads matter, and which is which differs from the browser (app/main.cpp):
//  * the process's FIRST thread runs the service. `init` and the shell know an app by its first thread (a
//    lifecycle Watch from another thread is refused with ESRCH, as is a notification), so the registry name
//    app.os.lazy.webd.light and the line to `init` live there;
//  * a thread made here runs WebKit: its run loop, the host and the views. The Rust half calls webd_engine_*
//    (any thread, each only queues work for the loop) and the engine calls webd_ui_* back on this thread.
//
// The loop ends when the service has stopped (init's Quit, SIGTERM) and asked it to (webd_engine_quit). The
// engine processes are then ended and waited for (CHILD:<name>:<pid>:GONE, engine markers) and the program
// exits with _exit, never a return: a return would run the host's destructors, and releasing a page proxy
// aborts. A WebKit thread that stopped answering cannot be asked to quit: the service says so and the program
// ends at once (exit status 4); its clients see their channels close.
//
// Markers are the Rust half's (WEBD:UP:PASS, WEBD:SERVE:..., WEBD:VIEW:..., WEBD:QUIT:PASS) and the engine's
// own, which reach the same trace through webd_ui_say.

#include <WebKit/WKRunLoop.h>

#include <atomic>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ucontext.h>
#include <unistd.h>

#include "engine.h"
#include "host.h"
#include "webd_abi.h"

extern "C" const char __ehdr_start;

namespace {

// A crash leaves no message anywhere on this system: print the signal, where it was and every word of the first
// stack kilobytes that points into the program's code, for `addr2line -f -C -e <the unstripped program> <at>`
// (the program is not position independent: the words are addresses as they are). The same idea as app/main.cpp,
// in one buffer per line and without allocation; it runs on its own stack and is reset on entry.
constexpr int kScanWords = 2048;
constexpr int kScanPrint = 48;

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

void putLine(const char* name, unsigned long a, const char* second = nullptr, unsigned long b = 0)
{
    char line[160];
    char* at = putText(line, "WEBD:CRASH:");
    at = putText(at, name);
    at = putText(at, "=");
    at = putHex(at, a);
    if (second) {
        at = putText(at, second);
        at = putHex(at, b);
    }
    *at++ = '\n';
    ssize_t ignored = write(STDOUT_FILENO, line, static_cast<size_t>(at - line));
    (void)ignored;
}

void reportCrash(int signal, siginfo_t* info, void* context)
{
    if (signal == SIGABRT) {
        for (int number : { SIGSEGV, SIGILL, SIGTRAP, SIGBUS, SIGFPE })
            ::signal(number, SIG_DFL);
    }
    const unsigned long base = reinterpret_cast<unsigned long>(&__ehdr_start);
    auto* uc = static_cast<ucontext_t*>(context);
    const unsigned long rsp = static_cast<unsigned long>(uc->uc_mcontext.gregs[REG_RSP]);
    const unsigned long rip = static_cast<unsigned long>(uc->uc_mcontext.gregs[REG_RIP]);
    putLine("signal", static_cast<unsigned long>(signal), ":base=", base);
    putLine("rip", rip, ":addr=", reinterpret_cast<unsigned long>(info ? info->si_addr : nullptr));
    // To the end of rsp's page at most when the stack is shorter than the scan: that page is mapped.
    const unsigned long* words = reinterpret_cast<const unsigned long*>(rsp);
    const unsigned long available = ((rsp | 4095UL) + 1 - rsp) / sizeof(unsigned long);
    const int limit = available < static_cast<unsigned long>(kScanWords) ? static_cast<int>(available) : kScanWords;
    int printed = 0;
    for (int i = 0; i < limit && printed < kScanPrint; ++i) {
        const unsigned long word = words[i];
        if (word > base && word < base + 0x8000000UL) {
            putLine("at", word);
            ++printed;
        }
    }
    if (signal == SIGABRT) {
        sigset_t pending;
        sigemptyset(&pending);
        sigaddset(&pending, SIGABRT);
        sigprocmask(SIG_UNBLOCK, &pending, nullptr);
        raise(SIGABRT);
        _exit(134);
    }
}

// One alternate stack per thread that installs the handler (a stack overflow is one of the crashes).
char serviceCrashStack[64 * 1024];
char webkitCrashStack[64 * 1024];

void installCrashHandler(char* stack, size_t size)
{
    stack_t alternate = { };
    alternate.ss_sp = stack;
    alternate.ss_size = size;
    sigaltstack(&alternate, nullptr);
    struct sigaction action = { };
    action.sa_sigaction = reportCrash;
    action.sa_flags = SA_SIGINFO | SA_ONSTACK | SA_RESETHAND;
    for (int number : { SIGSEGV, SIGILL, SIGTRAP, SIGBUS, SIGFPE, SIGABRT })
        sigaction(number, &action, nullptr);
}

// How the WebKit thread's start went: 0 not yet, 1 the host runs, -1 it could not start.
std::atomic<int> engineState { 0 };
// The engine processes left after the loop ended and they were ended (-1: the loop has not ended).
std::atomic<int> surviving { -1 };

void* webkitThread(void*)
{
    installCrashHandler(webkitCrashStack, sizeof webkitCrashStack);
    WKRunLoopInitializeMain();
    if (webd_engine_start() != 0) {
        engineState = -1;
        return nullptr;
    }
    // The first thing the loop runs: from here on it is turning.
    WKRunLoopCallOnMainThread([](void*) { webkitapp::H.running = true; }, nullptr);
    engineState = 1;
    WKRunLoopRunMain();
    // End the WebProcess and the NetworkProcess ourselves and wait for them: the sockets closing is not
    // something to rely on, and a process left behind holds its 107 MB and its share of the user's memory.
    surviving = webkitapp::shutdownChildren();
    return nullptr;
}

// init stops an app with Quit; SIGTERM is the same request from anyone else (kill, the debug tools).
void onTerminate(int) { webd_host_signal_quit(); }

} // namespace

int main(int, char**)
{
    setvbuf(stdout, nullptr, _IOLBF, 0);
    installCrashHandler(serviceCrashStack, sizeof serviceCrashStack);
    // A WebProcess or NetworkProcess that dies closes its end of the IPC socket; the next write must be EPIPE,
    // not a SIGPIPE that ends the program. (A C++ main does not get the Rust runtime's ignore of it.)
    signal(SIGPIPE, SIG_IGN);
    struct sigaction terminate = { };
    terminate.sa_handler = onTerminate;
    sigaction(SIGTERM, &terminate, nullptr);

    if (webd_host_init() != 0)
        _exit(1);

    pthread_attr_t attributes;
    pthread_attr_init(&attributes);
    // WebKit runs on this stack as it would on a main thread's: the 128 KiB default of a musl thread is too small.
    pthread_attr_setstacksize(&attributes, 8u << 20);
    pthread_t webkit;
    if (pthread_create(&webkit, &attributes, webkitThread, nullptr) != 0) {
        webd_host_fail("cannot start the WebKit thread");
        _exit(1);
    }
    // The service may not take a request before the host exists: wait for the thread to say how it went.
    for (int waited = 0; engineState == 0 && waited < 60000; waited += 5)
        usleep(5000);
    if (engineState != 1) {
        webd_host_fail(engineState < 0 ? "webd_engine_start failed" : "WebKit did not start within 60 s");
        _exit(1);
    }

    const int status = webd_host_main();
    // The service is done; end the run loop (a hung one cannot be asked, and is not waited for).
    if (status == 4) {
        webd_host_report(status, -1);
        // init restarts webd: the engine's processes of this one must not stay behind (107 MB each).
        webkitapp::killChildrenNow();
        _exit(status);
    }
    webd_engine_quit();
    pthread_join(webkit, nullptr);
    webd_host_report(status, surviving);
    fflush(stdout);
    _exit(status != 0 ? status : (surviving != 0 ? 3 : 0));
}
