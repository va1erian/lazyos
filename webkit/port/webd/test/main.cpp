// webd-abi-test: webd's engine half (webkit/port/webd/abi.cpp) driven through webd_abi.h by a program that
// plays the service (docs/webd-plan.md, P2 slice 1). WebKit's run loop owns the main thread, as in webd; a
// driver thread calls webd_engine_* like the Rust service's thread and waits for the webd_ui_* callbacks
// (support.cpp). One mode per process, so each starts with a host of its own:
//
//   webd-abi-test html | script | config | lock | network | many | lost | cycle
//
// Each prints its facts after "WEBD:" (the engine's own markers too) and ends with WEBD:<mode>:PASS, or
// WEBD:<mode>:FAIL:<why>. Exit status 0 on PASS with no engine process left behind.

#include <WebKit/WKRunLoop.h>

#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ucontext.h>
#include <unistd.h>

#include "engine.h"
#include "support.h"

extern "C" const char __ehdr_start;

namespace {

// A crash leaves no message: print the signal and every word of the first stack kilobytes that points into
// the program's code, for `addr2line -f -C -e <the unstripped program> <at>` (the program is not position
// independent). The same idea as webkit-views' and the app's handlers.
void reportCrash(int signal, siginfo_t*, void* context)
{
    const unsigned long base = reinterpret_cast<unsigned long>(&__ehdr_start);
    auto* uc = static_cast<ucontext_t*>(context);
    const unsigned long* words = reinterpret_cast<const unsigned long*>(uc->uc_mcontext.gregs[REG_RSP]);
    char line[64];
    int printed = 0;
    int length = snprintf(line, sizeof line, "WEBD:CRASH:signal=%d:base=%lx\n", signal, base);
    ssize_t ignored = write(STDOUT_FILENO, line, static_cast<size_t>(length));
    for (int i = 0; i < 4096 && printed < 40; ++i) {
        const unsigned long word = words[i];
        if (word > base && word < base + 0x8000000UL) {
            length = snprintf(line, sizeof line, "WEBD:CRASH:at=%lx\n", word);
            ignored = write(STDOUT_FILENO, line, static_cast<size_t>(length));
            ++printed;
        }
    }
    (void)ignored;
    _exit(134);
}

char crashStack[64 * 1024];

const char* mode = "";
std::string (*run)() = nullptr;
bool passed = false;

struct Named {
    const char* name;
    std::string (*run)();
};

const Named modes[] = {
    { "html", webdtest::modeHtml },
    { "script", webdtest::modeScript },
    { "config", webdtest::modeConfig },
    { "lock", webdtest::modeLock },
    { "network", webdtest::modeNetwork },
    { "many", webdtest::modeMany },
    { "lost", webdtest::modeLost },
    { "cycle", webdtest::modeCycle },
};

// The service's thread: runs the mode, says how it went and ends the run loop.
void* driver(void*)
{
    const std::string why = run();
    passed = why.empty();
    if (passed)
        webdtest::note("%s:PASS", mode);
    else
        webdtest::note("%s:FAIL:%s", mode, why.c_str());
    webd_engine_quit();
    return nullptr;
}

pthread_t mainThread;

// A mode that hangs is a failure, not a stuck build. The main thread is made to crash first, so the handler
// above says where it was stopped.
void* watchdog(void*)
{
    sleep(240);
    webdtest::note("%s:FAIL:timeout after 240 s", mode);
    pthread_kill(mainThread, SIGABRT);
    sleep(5);
    _exit(1);
    return nullptr;
}

} // namespace

int main(int argc, char** argv)
{
    for (const Named& named : modes) {
        if (argc > 1 && !strcmp(argv[1], named.name)) {
            mode = named.name;
            run = named.run;
        }
    }
    if (!run) {
        printf("usage: webd-abi-test html|script|config|lock|network|many|lost|cycle\n");
        return 2;
    }
    setvbuf(stdout, nullptr, _IOLBF, 0);
    signal(SIGPIPE, SIG_IGN);
    stack_t alternate = { };
    alternate.ss_sp = crashStack;
    alternate.ss_size = sizeof crashStack;
    sigaltstack(&alternate, nullptr);
    struct sigaction action = { };
    action.sa_sigaction = reportCrash;
    action.sa_flags = SA_SIGINFO | SA_ONSTACK | SA_RESETHAND;
    for (int number : { SIGABRT, SIGSEGV, SIGILL, SIGTRAP, SIGBUS })
        sigaction(number, &action, nullptr);

    mainThread = pthread_self();
    WKRunLoopInitializeMain();
    if (webd_engine_start() != 0) {
        webdtest::note("%s:FAIL:webd_engine_start failed", mode);
        _exit(1);
    }
    pthread_attr_t attributes;
    pthread_attr_init(&attributes);
    pthread_attr_setstacksize(&attributes, 1u << 20);
    pthread_t thread;
    pthread_create(&thread, &attributes, driver, nullptr);
    pthread_create(&thread, &attributes, watchdog, nullptr);
    WKRunLoopRunMain();
    // The engine processes must be gone after the run: shutdownChildren ends and counts any that are not.
    const int surviving = webkitapp::shutdownChildren();
    webdtest::note("CHILDREN:left=%d", surviving);
    fflush(stdout);
    _exit(passed && surviving == 0 ? 0 : 1);
}
