// store: a client's own in-memory profile and the same client's private window are different data stores,
// although both have no profile directory, the client's one group and memory_only (review of 2026-10-22,
// finding 1). Without webd_view_config.store_kind in the engine's group-mate match, the private window was a
// related page of the client's own view and sent its cookies.

#include "support.h"

#include <unistd.h>

#include "server.h"

namespace webdtest {

namespace {

Response html(const std::string& body) { return Response { 200, "text/html; charset=utf-8", {}, body }; }

} // namespace

std::string modeStore()
{
    const std::string marker = std::to_string(getpid()) + "x" + std::to_string(monotonicMs());
    const std::string ownValue = "own" + marker, privateValue = "private" + marker;
    Server server([&](const Request& r) {
        if (r.path == "/set-own" || r.path == "/set-private") {
            Response response = html("<!doctype html><title>set</title>");
            response.headers.push_back("Set-Cookie: " + std::string(r.path == "/set-own" ? "oc=" + ownValue : "pc=" + privateValue) + "; Path=/");
            return response;
        }
        if (r.path == "/check")
            return html("<!doctype html><title>check</title>");
        return Response { 404, "text/plain", {}, "missing" };
    });
    if (!server.start())
        return "the server did not start";
    const std::string origin = server.origin();
    setAnswers(Answers {});

    // All four views are the same client's: one non-zero group, no profile directory, memory_only.
    auto make = [](int storeKind) {
        FullOptions options;
        options.memoryOnly = 1;
        options.group = 7;
        options.storeKind = storeKind;
        return createFull(options);
    };
    // Loads `path` in `view` and returns the Cookie header the request carried.
    auto fetch = [&](uint64_t view, const std::string& path, const std::string& title, std::string& cookie) -> std::string {
        const int before = server.count(path);
        const int finished = facts(view).finished;
        webd_engine_load(view, (origin + path).c_str());
        if (!waitView(view, [&](const ViewFacts& f) { return f.finished > finished && f.title == title && !f.loading; }, 60000)
            || server.count(path) <= before)
            return "'" + path + "' did not load: " + describeFull(facts(view));
        bool found = false;
        cookie = server.last(path, found).cookie;
        return "";
    };

    std::string cookie, why;
    const uint64_t own1 = make(WEBD_STORE_OWN);
    if (!(why = fetch(own1, "/set-own", "set", cookie)).empty())
        return why;
    // The control: another view of the client's own profile, in the same group, sees the cookie. Without it the
    // test could pass because a cookie never arrives at all.
    const uint64_t own2 = make(WEBD_STORE_OWN);
    if (!(why = fetch(own2, "/check", "check", cookie)).empty())
        return why;
    note("STORE:own profile, second view:'%s'", cookie.c_str());
    if (cookie.find("oc=" + ownValue) == std::string::npos)
        return "a second view of the client's own profile did not send its cookie: '" + cookie + "'";

    // The private window, same client and group: nothing of the client's own profile.
    const uint64_t private1 = make(WEBD_STORE_PRIVATE);
    if (!(why = fetch(private1, "/check", "check", cookie)).empty())
        return why;
    note("STORE:private window:'%s'", cookie.c_str());
    if (!cookie.empty())
        return "the private window sent the client's own profile's cookie: '" + cookie + "'";

    // A private window sets its own; another private window of the client sees it (they share), the client's
    // own profile does not.
    if (!(why = fetch(private1, "/set-private", "set", cookie)).empty())
        return why;
    const uint64_t private2 = make(WEBD_STORE_PRIVATE);
    if (!(why = fetch(private2, "/check", "check", cookie)).empty())
        return why;
    note("STORE:second private window:'%s'", cookie.c_str());
    if (cookie.find("pc=" + privateValue) == std::string::npos)
        return "a second private window did not send the first one's cookie: '" + cookie + "'";
    if (cookie.find("oc=") != std::string::npos)
        return "a private window sent the client's own profile's cookie: '" + cookie + "'";
    if (!(why = fetch(own2, "/check", "check", cookie)).empty())
        return why;
    note("STORE:own profile after the private cookie:'%s'", cookie.c_str());
    if (cookie.find("pc=") != std::string::npos)
        return "the client's own profile sent a private window's cookie: '" + cookie + "'";
    if (cookie.find("oc=" + ownValue) == std::string::npos)
        return "the client's own profile lost its cookie: '" + cookie + "'";
    for (uint64_t id : { own1, own2, private1, private2 })
        destroy(id);
    return "";
}

} // namespace webdtest
