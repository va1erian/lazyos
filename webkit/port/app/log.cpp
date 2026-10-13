// Markers and text helpers for the UI process (docs/webkit-port-plan.md, W5).

#include <WebKit/WKString.h>
#include <WebKit/WKType.h>
#include <WebKit/WKURL.h>

#include <stdarg.h>
#include <stdio.h>
#include <time.h>

#include "engine.h"

namespace webkitapp {

void say(const char* format, ...)
{
    va_list args;
    va_start(args, format);
    vprintf(format, args);
    va_end(args);
    printf("\n");
    fflush(stdout);
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
