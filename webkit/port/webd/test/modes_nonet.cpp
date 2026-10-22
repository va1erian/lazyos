// nonet: a full view made with no_network (a client without the network scope) reaches no network by any road,
// as a light view: subresources, script requests and the main frame's own loads, whatever webd_ui_navigate says.
// A control view of the same page without the lock proves the server sees what a view loads.

#include "support.h"

#include <atomic>
#include <unistd.h>

#include "server.h"

namespace webdtest {

namespace {

Response html(const std::string& body) { return Response { 200, "text/html; charset=utf-8", {}, body }; }

// A page that asks the server for everything a client page could: a style sheet import, a picture, fetch, XHR, a
// picture made by script; and a link to the server's own page over the top left 200x100 pixels.
std::string roads(const std::string& origin)
{
    return "<!doctype html><title>?</title><style>@import url('" + origin + "/import.css');</style>"
        "<body style=\"margin:0;background:#fff\"><a href=\"" + origin + "/page\" style=\"display:block;position:absolute;left:0;top:0;"
        "width:200px;height:100px;background:#f00\">go</a><img src=\"" + origin + "/img.png\"><script>"
        "fetch('" + origin + "/fetch').catch(function() {});"
        "var x = new XMLHttpRequest(); x.open('GET', '" + origin + "/xhr'); x.send();"
        "new Image().src = '" + origin + "/jsimg';"
        "setTimeout(function() { document.title = 'done' }, 600)</script>";
}

} // namespace

std::string modeNonet()
{
    Server server([](const Request& r) {
        if (r.path == "/page")
            return html("<!doctype html><title>Remote</title>");
        return Response { 200, "text/plain", {}, "x" };
    });
    if (!server.start())
        return "the server did not start";
    const std::string origin = server.origin();
    // The verdict for navigations to the server, changed as the mode goes; everything else is allowed.
    static std::atomic<int> verdict { WEBD_VERDICT_DENY };
    setAnswers(Answers { [origin](uint64_t, const Navigation& n) {
        return n.url.compare(0, origin.size(), origin) == 0 ? verdict.load() : WEBD_VERDICT_ALLOW;
    }, nullptr, "" });

    // The control: the same page in a full view with the network.
    FullOptions options;
    options.features = WEBD_FEATURE_POLICY | WEBD_FEATURE_PAGE_UI;
    const uint64_t control = createFull(options);
    loadHtml(control, roads(origin), "about:blank");
    if (!waitView(control, [](const ViewFacts& f) { return f.title == "done"; }, 60000) || !server.wait("/", 4, 30000))
        return "the control view did not reach the server: the test proves nothing (" + server.describe() + ")";
    note("NONET:control reached the server:%s", server.describe().c_str());
    destroy(control);
    usleep(1000 * 1000);
    const int baseline = static_cast<int>(server.requests().size());

    // no_network with content blocking is refused.
    FullOptions blocked = options;
    blocked.noNetwork = 1;
    blocked.features |= WEBD_FEATURE_BLOCKING;
    blocked.profile = scratchFolder("nonet") + "/profile";
    const uint64_t refused = createFull(blocked);
    if (!waitView(refused, [](const ViewFacts& f) { return f.lost; }, 30000) || facts(refused).lostReason != WEBD_LOST_REFUSED)
        return "no_network with blocking was not refused";

    options.noNetwork = 1;
    const uint64_t v = createFull(options);
    loadHtml(v, roads(origin), "about:blank");
    if (!waitView(v, [](const ViewFacts& f) { return f.title == "done" && !f.loading; }, 60000))
        return "the page did not run: " + describeFull(facts(v));
    usleep(2000 * 1000);
    if (static_cast<int>(server.requests().size()) != baseline)
        return "a view without network reached the server: " + server.describe();
    note("NONET:subresources and script requests:no request");

    // A link the service refuses: asked, nothing requested.
    auto clickAsked = [&]() -> bool {
        const size_t before = facts(v).navigations.size();
        click(v, 50, 50);
        return waitView(v, [&](const ViewFacts& f) {
            return f.navigations.size() > before && f.navigations.back().url == origin + "/page";
        }, 30000);
    };
    if (!clickAsked())
        return "the link click was not asked about";
    usleep(1500 * 1000);
    if (static_cast<int>(server.requests().size()) != baseline || facts(v).title != "done")
        return "a refused link reached the server: " + server.describe();
    note("NONET:denied link:asked, no request");

    // A link the service allows: the lock blocks the document load itself.
    verdict = WEBD_VERDICT_ALLOW;
    if (!clickAsked())
        return "the allowed link click was not asked about";
    waitView(v, [](const ViewFacts& f) { return !f.loading && !f.error.empty(); }, 10000);
    usleep(1500 * 1000);
    ViewFacts f = facts(v);
    note("NONET:allowed link:requests=%d:title=%s:committed=%s:error=%s", static_cast<int>(server.requests().size()) - baseline,
        f.title.c_str(), f.committed.c_str(), f.error.c_str());
    if (static_cast<int>(server.requests().size()) != baseline)
        return "an allowed main-frame navigation reached the server: " + server.describe();
    if (f.title == "Remote")
        return "an allowed main-frame navigation loaded the remote page";

    // ... and a load the service asks for.
    webd_engine_load(v, (origin + "/page").c_str());
    waitView(v, [](const ViewFacts& g) { return !g.loading && !g.error.empty(); }, 10000);
    usleep(1500 * 1000);
    f = facts(v);
    note("NONET:allowed load:requests=%d:title=%s:error=%s", static_cast<int>(server.requests().size()) - baseline, f.title.c_str(),
        f.error.c_str());
    if (static_cast<int>(server.requests().size()) != baseline || f.title == "Remote")
        return "an allowed load reached the server: " + server.describe();
    // The view still works for what it may load.
    loadHtml(v, page("Inert", "#0a0"), "about:blank");
    if (!waitView(v, [](const ViewFacts& g) { return g.title == "Inert" && !g.loading; }, 30000))
        return "the view did not load an inert page after the refusals: " + describeFull(facts(v));
    destroy(v);
    return "";
}

} // namespace webdtest
