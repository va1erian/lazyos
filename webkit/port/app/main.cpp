// webkit-app: WebKit in a desktop window (docs/webkit-port-plan.md, W5).
//
// The UI process has two threads that matter: WebKit's run loop on the process's main thread, as
// in webkit-smoke (engine.cpp), and the xui window on a second one (a Rust static library,
// xui-app/crates/webkit, through lazyos_ui.h). A Messenger handle belongs to the thread that made
// it, so the window is built, used and destroyed on its own thread. When the window closes the run
// loop ends, the UI process ends the WebProcess and the NetworkProcess (SIGTERM, SIGKILL after a
// grace period), waits for them (WEBKIT:W5:CHILD:<name>:<pid>:GONE) and exits.
//
//   WEBKIT:W5:UP                 the UI process started and WebKit2 initialised
//   WEBKIT:W5:CHILD:<name>:<pid>:GONE   a child process ended (ALIVE: it did not)
//   WEBKIT:W5:QUIT:PASS          the window closed, the run loop ended and the children are gone
//   (and the markers of engine.cpp and the window's WEBKIT:W5:UI:*)

#include <WebKit/WKRunLoop.h>

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

#include "lazyos_ui.h"

namespace {

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
    WKRunLoopInitializeMain();
    printf("WEBKIT:W5:UP\n");
    fflush(stdout);
    if (lzw_engine_start() != 0)
        return 1;

    static Arguments arguments { argc, argv, 0 };
    pthread_attr_t attributes;
    pthread_attr_init(&attributes);
    // xui lays out and shapes text on this stack: the 128 KiB default of a musl thread is too small.
    pthread_attr_setstacksize(&attributes, 8u << 20);
    pthread_t window;
    if (pthread_create(&window, &attributes, windowThread, &arguments) != 0) {
        printf("WEBKIT:W5:FAIL:window:cannot start the window's thread\n");
        return 1;
    }
    pthread_detach(window);

    WKRunLoopRunMain();
    // End the WebProcess and the NetworkProcess ourselves and wait for them: the sockets closing
    // is not something to rely on, and a process left behind holds its 107 MB and its share of the
    // user's memory quota.
    const int surviving = lzw_engine_shutdown();
    printf("WEBKIT:W5:QUIT:%s\n", arguments.status == 0 && surviving == 0 ? "PASS" : "FAIL");
    fflush(stdout);
    _exit(arguments.status);
}
