// webkit-app: WebKit in a desktop window (docs/webkit-port-plan.md, W5).
//
// The UI process has two threads that matter: WebKit's run loop (engine.cpp) and the xui window
// (a Rust static library, xui-app/crates/webkit, through lazyos_ui.h). By default the run loop
// owns the process's main thread, as in webkit-smoke, and the window runs on a second one;
// `--ui-main` swaps them (the window on the main thread, the run loop on a second one). When
// the window closes the run loop ends and the process exits, which closes the IPC sockets, so
// the WebProcess and NetworkProcess follow.
//
//   WEBKIT:W5:UP                 the UI process started and WebKit2 initialised
//   WEBKIT:W5:QUIT:PASS          the window closed and the run loop ended
//   (and the markers of engine.cpp and the window's WEBKIT:W5:UI:*)

#include <WebKit/WKRunLoop.h>

#include <atomic>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "lazyos_ui.h"

namespace {

struct Arguments {
    int argc;
    char** argv;
    int status;
};

// The window's thread: the window is built, used and destroyed here (a Messenger handle
// belongs to the thread that made it).
void* windowThread(void* context)
{
    auto* arguments = static_cast<Arguments*>(context);
    arguments->status = lzw_ui_main(arguments->argc, arguments->argv);
    lzw_engine_quit();
    return nullptr;
}

// With --ui-main: the run loop's thread. 1 once the engine is up, -1 when it could not start.
std::atomic<int> engineState { 0 };

void* runLoopThread(void*)
{
    WKRunLoopInitializeMain();
    printf("WEBKIT:W5:UP\n");
    fflush(stdout);
    engineState = lzw_engine_start() == 0 ? 1 : -1;
    if (engineState == 1)
        WKRunLoopRunMain();
    return nullptr;
}

bool startThread(void* (*entry)(void*), void* argument, pthread_t* thread)
{
    pthread_attr_t attributes;
    pthread_attr_init(&attributes);
    // xui lays out and shapes text on a stack: the 128 KiB default of a musl thread is too small.
    pthread_attr_setstacksize(&attributes, 8u << 20);
    return pthread_create(thread, &attributes, entry, argument) == 0;
}

bool hasOption(int argc, char** argv, const char* option)
{
    for (int i = 1; i < argc; ++i) {
        if (!strcmp(argv[i], option))
            return true;
    }
    return false;
}

} // namespace

int main(int argc, char** argv)
{
    setvbuf(stdout, nullptr, _IOLBF, 0);
    static Arguments arguments { argc, argv, 0 };

    if (hasOption(argc, argv, "--ui-main")) {
        pthread_t loop;
        if (!startThread(runLoopThread, nullptr, &loop)) {
            printf("WEBKIT:W5:FAIL:window:cannot start the run loop's thread\n");
            return 1;
        }
        while (engineState == 0)
            usleep(5000);
        if (engineState < 0)
            return 1;
        arguments.status = lzw_ui_main(argc, argv);
        lzw_engine_quit();
        pthread_join(loop, nullptr);
        printf("WEBKIT:W5:QUIT:%s\n", arguments.status == 0 ? "PASS" : "FAIL");
        fflush(stdout);
        _exit(arguments.status);
    }

    WKRunLoopInitializeMain();
    printf("WEBKIT:W5:UP\n");
    fflush(stdout);
    if (lzw_engine_start() != 0)
        return 1;

    pthread_t window;
    if (!startThread(windowThread, &arguments, &window)) {
        printf("WEBKIT:W5:FAIL:window:cannot start the window's thread\n");
        return 1;
    }
    pthread_detach(window);

    WKRunLoopRunMain();
    printf("WEBKIT:W5:QUIT:%s\n", arguments.status == 0 ? "PASS" : "FAIL");
    fflush(stdout);
    _exit(arguments.status);
}
