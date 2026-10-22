// webd-abi-test: webd's engine half (webkit/port/webd/abi.cpp) driven through webd_abi.h by a program that
// plays the service (docs/webd-plan.md, P2 slice 1). WebKit's run loop owns the main thread, as in webd; a
// driver thread calls webd_engine_* like the Rust service's thread and waits for the webd_ui_* callbacks
// (support.cpp). One mode per process, so each starts with a host of its own:
//
//   webd-abi-test html | script | config | lock | network | many | lost | cycle            (light views)
//   webd-abi-test full | profiles | ask | dialog | download | popup | deltas | blocking | notify | nonet | store  (full views)
//
// The light modes start the host with webd_engine_start, as slice 1's service did; the full ones with
// webd_engine_start_host and a children's folder of their own, against HTTP servers the test runs on
// 127.0.0.1 (support_full.cpp, server.cpp, modes_full.cpp, modes_page.cpp, modes_paint.cpp).
//
// Each prints its facts after "WEBD:" (the engine's own markers too) and ends with WEBD:<mode>:PASS, or
// WEBD:<mode>:FAIL:<why>. Exit status 0 on PASS with no engine process left behind.

#include <WebKit/WKRunLoop.h>

#include <filesystem>
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
    bool full;
};

const Named modes[] = {
    { "html", webdtest::modeHtml, false },
    { "script", webdtest::modeScript, false },
    { "config", webdtest::modeConfig, false },
    { "lock", webdtest::modeLock, false },
    { "network", webdtest::modeNetwork, false },
    { "many", webdtest::modeMany, false },
    { "lost", webdtest::modeLost, false },
    { "cycle", webdtest::modeCycle, false },
    { "full", webdtest::modeFull, true },
    { "profiles", webdtest::modeProfiles, true },
    { "ask", webdtest::modeAsk, true },
    { "dialog", webdtest::modeDialog, true },
    { "download", webdtest::modeDownload, true },
    { "popup", webdtest::modePopup, true },
    { "deltas", webdtest::modeDeltas, true },
    { "blocking", webdtest::modeBlocking, true },
    { "notify", webdtest::modeNotify, true },
    { "nonet", webdtest::modeNonet, true },
    { "store", webdtest::modeStore, true },
};
bool fullHost = false;

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
            fullHost = named.full;
        }
    }
    if (!run) {
        printf("usage: webd-abi-test html|script|config|lock|network|many|lost|cycle|full|profiles|ask|dialog|download|popup|deltas|blocking|notify|nonet|store\n");
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

    // The engine's network lock keeps its rule store in a folder the owner names (webd: its profile root).
    char rules[512];
    snprintf(rules, sizeof rules, "%s/webd-abi-test-XXXXXX", std::filesystem::temp_directory_path().c_str());
    if (!mkdtemp(rules)) {
        webdtest::note("%s:FAIL:no folder for the rule store", mode);
        _exit(1);
    }
    setenv("LAZYOS_WEBD_RULES_DIR", rules, 1);

    mainThread = pthread_self();
    WKRunLoopInitializeMain();
    int started = 1;
    if (fullHost) {
        // The children's folder: decided once, here, for every profile the mode makes.
        webdtest::childrenDirectory = std::string(rules) + "/children";
        webd_host_config host = { };
        host.struct_size = sizeof host;
        host.children_directory = webdtest::childrenDirectory.c_str();
        started = webd_engine_start_host(&host);
        // A second start of either kind is refused.
        if (started == 0 && (webd_engine_start() == 0 || webd_engine_start_host(&host) == 0)) {
            webdtest::note("%s:FAIL:a second host start was not refused", mode);
            _exit(1);
        }
    } else {
        started = webd_engine_start();
    }
    if (started != 0) {
        webdtest::note("%s:FAIL:the host did not start", mode);
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
