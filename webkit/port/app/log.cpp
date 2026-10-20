// Markers and text helpers for the UI process (docs/webkit-port-plan.md and LazyWebPlus,
// docs/lazywebplus.md). A marker is "<prefix>:<text>", the prefix being the window's
// (lzw_ui_marker_prefix: WEBPLUS for LazyWebPlus).

#include <WebKit/WKString.h>
#include <WebKit/WKType.h>
#include <WebKit/WKURL.h>

#include <stdarg.h>
#include <stdio.h>
#include <time.h>

#include "engine.h"
#include "lazyos_ui.h"

namespace webkitapp {

namespace {

// The whole line is formatted here and written by the window in one go (issue #891): say() runs on
// WebKit's main thread, the launcher's reaper threads and the window's, and a line printed in three
// writes could be torn by another. A line longer than the buffer is cut, never split.
void print(const char* format, va_list args)
{
    char line[1024];
    const int length = vsnprintf(line, sizeof line, format, args);
    if (length < 0)
        return;
    for (char* at = line; *at; ++at) {
        if (*at == '\n' || *at == '\r')
            *at = ' ';
    }
    lzw_ui_say(line);
}

} // namespace

void say(const char* format, ...)
{
    va_list args;
    va_start(args, format);
    print(format, args);
    va_end(args);
}

bool harness()
{
    static const bool on = lzw_ui_probe_enabled() != 0;
    return on;
}

bool has(int feature)
{
    static const int features = lzw_ui_features();
    return (features & feature) != 0;
}

void sayHarness(const char* format, ...)
{
    // A window that prints the page markers itself (LOAD, TITLE, ...) says them once.
    if (!harness() || has(LZW_FEATURE_OWN_MARKERS))
        return;
    va_list args;
    va_start(args, format);
    print(format, args);
    va_end(args);
}

long elapsedMs()
{
    static const long origin = [] {
        timespec now;
        clock_gettime(CLOCK_MONOTONIC, &now);
        return now.tv_sec * 1000L + now.tv_nsec / 1000000L;
    }();
    timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec * 1000L + now.tv_nsec / 1000000L - origin;
}

std::string utf8(WKStringRef string)
{
    if (!string)
        return std::string();
    size_t size = WKStringGetMaximumUTF8CStringSize(string);
    std::vector<char> buffer(size + 1);
    size_t written = WKStringGetUTF8CString(string, buffer.data(), buffer.size());
    return std::string(buffer.data(), written ? written - 1 : 0);
}

std::string utf8(WKURLRef url)
{
    if (!url)
        return std::string();
    WKStringRef string = WKURLCopyString(url);
    std::string text = utf8(string);
    if (string)
        WKRelease(string);
    return text;
}

std::string oneLine(std::string text)
{
    for (char& c : text) {
        if (c == '\n' || c == '\r')
            c = ' ';
    }
    return text;
}

} // namespace webkitapp
