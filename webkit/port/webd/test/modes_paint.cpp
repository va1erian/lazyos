// deltas and blocking: a full view that sends what changed, rebuilt by the test into the picture a whole frame
// must equal; and a profile's content rule list, judged by what the server was asked for.

#include "support.h"

#include <string.h>
#include <unistd.h>

#include "server.h"

namespace webdtest {

namespace {

Response html(const std::string& body) { return Response { 200, "text/html; charset=utf-8", {}, body }; }

// Rows of colour 50 pixels high and a line of text in each: every scroll position looks different.
std::string tallPage()
{
    static const char* const colours[] = { "#c00", "#0a0", "#00c", "#cc0", "#0cc", "#c0c", "#888", "#fff" };
    std::string body = "<!doctype html><title>tall</title><body style=\"margin:0;font:16px sans-serif\">";
    for (int i = 0; i < 80; ++i)
        body += std::string("<div style=\"height:50px;background:") + colours[i % 8] + "\">row " + std::to_string(i) + "</div>";
    return body;
}

// Waits until the view has had no post (frame or delta) for `quietMs`.
bool settle(uint64_t view, int quietMs, int ms)
{
    const long end = monotonicMs() + ms;
    int posts = -1;
    long since = monotonicMs();
    while (monotonicMs() < end) {
        const ViewFacts f = facts(view);
        if (f.frames + f.deltas != posts) {
            posts = f.frames + f.deltas;
            since = monotonicMs();
        } else if (monotonicMs() - since >= quietMs) {
            return true;
        }
        usleep(50 * 1000);
    }
    return false;
}

} // namespace

std::string modeDeltas()
{
    Server server([](const Request&) { return html(tallPage()); });
    if (!server.start())
        return "the server did not start";
    setAnswers(Answers {});
    FullOptions options;
    options.deltas = 1;
    options.width = 400;
    options.height = 300;
    const uint64_t v = createFull(options);
    webd_engine_load(v, (server.origin() + "/tall").c_str());
    if (!waitPage(v, "tall") || !waitView(v, [](const ViewFacts& f) { return f.frames > 0; }, 30000) || !settle(v, 800, 30000))
        return "the tall page did not show: " + describeFull(facts(v));
    // Up at the top: the page cannot go there, and still answers the turn, where it stayed (WebKit patch 0035).
    // Which sign is up is WebKit's, so both are turned once, each numbered.
    uint64_t seq = 0;
    webd_engine_wheel(v, 200, 150, 0, 1, 0, ++seq);
    if (!waitView(v, [&](const ViewFacts& f) { return f.wheelDoneSeq == seq; }, 10000))
        return "no answer to the first wheel turn: " + describeFull(facts(v));
    // A wheel turn down. (Which sign is "down" is WebKit's: try one, then the other.)
    int down = 0;
    for (int ticks : { -3, 3 }) {
        webd_engine_wheel(v, 200, 150, 0, ticks, 0, ++seq);
        if (waitView(v, [](const ViewFacts& f) { return f.movedDeltas > 0 && f.scrollY > 0; }, 5000)) {
            down = ticks;
            break;
        }
    }
    if (!down)
        return "no delta that moved the picture after a wheel turn: " + describeFull(facts(v));
    // The answer to the turn that scrolled names the new offset already: the page's process said it scrolled
    // before it answered (the order the window's picture ahead of the page relies on).
    if (!waitView(v, [&](const ViewFacts& f) { return f.wheelDoneSeq == seq; }, 10000))
        return "no answer to the turn that scrolled: " + describeFull(facts(v));
    if (facts(v).wheelDoneY <= 0)
        return "the answer to the turn that scrolled names the old offset: " + describeFull(facts(v));
    // A second turn while the first may still be painting.
    webd_engine_wheel(v, 200, 150, 0, down, 0, ++seq);
    if (!settle(v, 1000, 30000))
        return "the view did not stop painting";
    // Every turn answered, the newest last, with where the page is (the offset its frames are painted at).
    const ViewFacts answered = facts(v);
    note("DELTAS:answers=%d:seq=%llu/%llu:y=%d:scroll=%d", answered.wheelDones, static_cast<unsigned long long>(answered.wheelDoneSeq),
        static_cast<unsigned long long>(seq), answered.wheelDoneY, answered.scrollY);
    if (answered.wheelDoneSeq != seq || answered.wheelDoneY != answered.scrollY || answered.violations)
        return "the wheel's answers do not name the last turn and the page's offset: " + describeFull(answered);
    const ViewFacts rebuilt = facts(v);
    note("DELTAS:frames=%d:deltas=%d:moved=%d:bands=%d:scroll=%d,%d:unannounced=%d", rebuilt.frames, rebuilt.deltas, rebuilt.movedDeltas,
        rebuilt.bands, rebuilt.scrollX, rebuilt.scrollY, rebuilt.unannounced);
    if (rebuilt.unannounced)
        return std::to_string(rebuilt.unannounced) + " posts came without a scroll call before them";
    // A whole frame now: the picture the deltas built must be it, pixel for pixel.
    webd_engine_refresh(v);
    if (!waitView(v, [&](const ViewFacts& f) { return f.frames > rebuilt.frames; }, 30000))
        return "no whole frame after refresh";
    const ViewFacts whole = facts(v);
    if (whole.pictureWidth != rebuilt.pictureWidth || whole.pictureHeight != rebuilt.pictureHeight || whole.picture.size() != rebuilt.picture.size())
        return "the whole frame is not the picture's size";
    long differing = 0;
    int firstX = -1, firstY = -1;
    for (size_t i = 0; i < whole.picture.size(); i += 4) {
        if (memcmp(whole.picture.data() + i, rebuilt.picture.data() + i, 4)) {
            if (!differing++) {
                firstX = static_cast<int>(i / 4 % whole.pictureWidth);
                firstY = static_cast<int>(i / 4 / whole.pictureWidth);
            }
        }
    }
    note("DELTAS:rebuilt vs whole:%ld pixels differ (first %d,%d)", differing, firstX, firstY);
    if (differing)
        return "the picture rebuilt from the deltas is not the whole frame (" + std::to_string(differing) + " pixels)";
    if (whole.scrollY != rebuilt.scrollY)
        return "the whole frame was painted at another offset";
    destroy(v);
    return "";
}

std::string modeBlocking()
{
    Server server([](const Request& r) {
        if (r.path == "/page" || r.path == "/page2")
            return html("<!doctype html><title>" + r.path.substr(1) + "</title><body><img src=\"/blocked.png\"><img src=\"/ok.png\">");
        return Response { 200, "image/png", {}, "not really a picture" };
    });
    if (!server.start())
        return "the server did not start";
    // The lists apply to every page but /page2.
    setAnswers(Answers { nullptr, [](uint64_t, const std::string& url) { return url.find("/page2") == std::string::npos ? 1 : 0; }, "" });
    FullOptions options;
    options.features = WEBD_FEATURE_POLICY | WEBD_FEATURE_BLOCKING;
    options.profile = scratchFolder("blocking") + "/profile";
    const uint64_t v = createFull(options);
    const std::string rules = R"([{"trigger":{"url-filter":"blocked\\.png"},"action":{"type":"block"}}])";
    webd_engine_blocking_compile(v, "test", rules.data(), rules.size());
    auto listSaid = [](const ViewFacts& f, const char* list, int status) {
        for (const auto& said : f.blockingLists)
            if (said.first == list && said.second == status)
                return true;
        return false;
    };
    if (!waitView(v, [&](const ViewFacts& f) { return listSaid(f, "test", 0); }, 60000))
        return "the rule list did not compile";
    webd_engine_blocking_load(v, "nothing-stored");
    if (!waitView(v, [&](const ViewFacts& f) { return listSaid(f, "nothing-stored", 1); }, 30000))
        return "a list that was never stored was not reported as such";

    webd_engine_load(v, (server.origin() + "/page").c_str());
    if (!waitPage(v, "page") || !server.wait("/ok.png", 1, 30000))
        return "the page did not load its picture: " + server.describe();
    if (!waitView(v, [](const ViewFacts& f) { return f.blocked == 1; }, 30000))
        return "the blocked load was not counted: blocked=" + std::to_string(facts(v).blocked);
    usleep(1000 * 1000);
    if (server.count("/blocked.png"))
        return "the server was asked for the blocked picture: " + server.describe();
    note("BLOCKING:page:blocked=%d:%s", facts(v).blocked, server.describe().c_str());

    // The service says the lists do not apply to /page2: the picture is asked for.
    webd_engine_load(v, (server.origin() + "/page2").c_str());
    if (!waitPage(v, "page2") || !server.wait("/blocked.png", 1, 30000))
        return "the excepted page's picture was not asked for: " + server.describe();
    usleep(500 * 1000);
    if (facts(v).blocked != 0)
        return "the excepted page counted a blocked load";
    // Forgotten, the list no longer blocks /page either.
    webd_engine_blocking_forget(v, "test");
    webd_engine_load(v, (server.origin() + "/page").c_str());
    if (!waitView(v, [](const ViewFacts& f) { return f.finished >= 3 && !f.loading && f.title == "page"; }, 60000) || !server.wait("/blocked.png", 2, 30000))
        return "a forgotten list still blocked: " + server.describe();
    note("BLOCKING:excepted and forgotten:%s", server.describe().c_str());
    destroy(v);
    return "";
}

} // namespace webdtest
