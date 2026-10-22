// full, profiles and ask: a full view's navigation and page state, its profile's cookies, and a navigation held
// for the service's answer. Each runs HTTP servers of its own on 127.0.0.1 (server.h).

#include "support.h"

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "server.h"

namespace webdtest {

namespace {

Response html(const std::string& body) { return Response { 200, "text/html; charset=utf-8", {}, body }; }
Response missing() { return Response { 404, "text/plain", {}, "missing" }; }

// The navigation `view` was last asked about whose address starts with `prefix`, or null.
const Navigation* lastNavigation(const ViewFacts& f, const std::string& prefix)
{
    for (auto it = f.navigations.rbegin(); it != f.navigations.rend(); ++it) {
        if (it->url.compare(0, prefix.size(), prefix) == 0)
            return &*it;
    }
    return nullptr;
}

// Whether any file below `folder` holds `needle`; `where` names the first one.
bool folderHolds(const std::string& folder, const std::string& needle, std::string& where)
{
    std::error_code error;
    if (!std::filesystem::exists(folder, error))
        return false;
    for (auto it = std::filesystem::recursive_directory_iterator(folder, error); !error && it != std::filesystem::recursive_directory_iterator(); it.increment(error)) {
        if (!it->is_regular_file(error))
            continue;
        std::ifstream file(it->path(), std::ios::binary);
        const std::string bytes((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
        if (bytes.find(needle) != std::string::npos) {
            where = it->path().string();
            return true;
        }
    }
    return false;
}

} // namespace

std::string modeFull()
{
    Server server([](const Request& r) {
        if (r.path == "/one")
            return html(linkPage("One", "/two"));
        if (r.path == "/two")
            return html(linkPage("Two", ""));
        if (r.path == "/base/pic.png")
            return Response { 200, "image/png", {}, "not really a picture" };
        return missing();
    });
    if (!server.start())
        return "the server did not start";
    const std::string origin = server.origin();
    setAnswers(Answers { [](uint64_t, const Navigation& n) { return n.url.find("/denied") != std::string::npos ? WEBD_VERDICT_DENY : WEBD_VERDICT_ALLOW; }, nullptr, "" });
    FullOptions options;
    options.features = WEBD_FEATURE_POLICY | WEBD_FEATURE_PAGE_UI;
    options.userAgentSuffix = "WebdTest/1.0";
    const uint64_t v = createFull(options);
    // Success is the first page state.
    if (!waitView(v, [](const ViewFacts& f) { return f.pageStates >= 1; }, 60000))
        return "no page state after view_create: " + describeFull(facts(v));

    webd_engine_load(v, (origin + "/one").c_str());
    if (!waitView(v, [&](const ViewFacts& f) { return f.title == "One" && !f.loading && f.finished == 1 && f.committed == origin + "/one" && f.frames > 0; }, 60000))
        return "the first page did not load: " + describeFull(facts(v));
    ViewFacts f = facts(v);
    note("FULL:one:%s", describeFull(f).c_str());
    if (f.url != origin + "/one" || f.canBack || f.canForward || f.secure || !f.error.empty() || f.progress != 1000)
        return "the first page's state is wrong: " + describeFull(f);
    const Navigation* asked = lastNavigation(f, origin + "/one");
    if (!asked || !asked->mainFrame || asked->gesture || asked->kind != 5)
        return "the load was not asked about as a main-frame navigation of kind other without a gesture";
    bool found = false;
    const Request first = server.last("/one", found);
    note("FULL:user agent:%s", first.userAgent.c_str());
    const std::string suffix = " WebdTest/1.0";
    const std::string& agent = first.userAgent;
    if (!found || agent.size() <= suffix.size() || agent.compare(agent.size() - suffix.size(), suffix.size(), suffix) != 0)
        return "the user agent does not end with the suffix: '" + agent + "'";

    // A link click: the second page, with a history.
    click(v, 50, 50);
    if (!waitView(v, [&](const ViewFacts& g) { return g.title == "Two" && !g.loading && g.finished == 2 && g.canBack && !g.canForward; }, 60000))
        return "the link did not lead to the second page: " + describeFull(facts(v));
    asked = lastNavigation(facts(v), origin + "/two");
    if (!asked || asked->kind != 0 || !asked->gesture || asked->committed != origin + "/one")
        return "the click was not asked about as a link with a gesture from the first page";

    webd_engine_back(v);
    if (!waitView(v, [&](const ViewFacts& g) { return g.title == "One" && !g.loading && g.committed == origin + "/one" && !g.canBack && g.canForward; }, 60000))
        return "Back did not return to the first page: " + describeFull(facts(v));
    const int afterBack = facts(v).finished;
    webd_engine_forward(v);
    if (!waitView(v, [&](const ViewFacts& g) { return g.title == "Two" && !g.loading && g.canBack && !g.canForward; }, 60000))
        return "Forward did not return to the second page: " + describeFull(facts(v));
    const int beforeReload = facts(v).finished;
    webd_engine_reload(v);
    if (!waitView(v, [&](const ViewFacts& g) { return g.finished > beforeReload && !g.loading && g.title == "Two"; }, 60000))
        return "Reload did not finish a load: " + describeFull(facts(v));
    note("FULL:finished:after back=%d:before reload=%d:after=%d", afterBack, beforeReload, facts(v).finished);

    // A navigation the service refuses loads nothing; a load that fails says why and leaves the page.
    const int requests = static_cast<int>(server.requests().size());
    webd_engine_load(v, (origin + "/denied").c_str());
    usleep(1500 * 1000);
    if (server.count("/denied") || facts(v).title != "Two")
        return "a refused navigation loaded: " + server.describe();
    webd_engine_load(v, "http://127.0.0.1:1/nothing");
    if (!waitView(v, [](const ViewFacts& g) { return !g.error.empty() && !g.loading; }, 60000))
        return "a failed load said nothing: " + describeFull(facts(v));
    f = facts(v);
    note("FULL:error:%s", f.error.c_str());
    if (f.committed != origin + "/two" || f.title != "Two")
        return "a failed load replaced the page: " + describeFull(f);
    // A view made with deltas = 0 gets whole frames only.
    if (f.deltas || f.movedDeltas)
        return "a view without deltas got a delta";
    note("FULL:requests=%d:%s", requests, server.describe().c_str());
    // The client's own page under the server's address: its relative picture is asked for there.
    const std::string base = origin + "/base/";
    int finished = facts(v).finished;
    loadHtml(v, page("Html", "#00f", "<img src=\"pic.png\">"), base.c_str());
    if (!waitView(v, [&](const ViewFacts& g) { return g.title == "Html" && !g.loading && g.finished > finished && g.error.empty(); }, 60000))
        return "load_html with an http base did not show: " + describeFull(facts(v));
    if (!server.wait("/base/pic.png", 1, 30000))
        return "the page's relative picture was not asked for under its base: " + server.describe();
    const Navigation* htmlAsked = lastNavigation(facts(v), base);
    note("FULL:load_html:http base:committed=%s:asked=%s", facts(v).committed.c_str(), htmlAsked ? htmlAsked->url.c_str() : "(none)");
    if (!htmlAsked || !htmlAsked->mainFrame)
        return "load_html's navigation was not asked about";
    // ... and under about:blank.
    finished = facts(v).finished;
    loadHtml(v, page("Blank", "#0a0"), "about:blank");
    if (!waitView(v, [&](const ViewFacts& g) { return g.title == "Blank" && !g.loading && g.finished > finished && g.error.empty(); }, 60000))
        return "load_html with about:blank did not show: " + describeFull(facts(v));
    // A file: base is refused: nothing loads and the state says why.
    const int states = facts(v).pageStates;
    loadHtml(v, page("File", "#f00"), "file:///etc/");
    if (!waitView(v, [&](const ViewFacts& g) { return g.pageStates > states && !g.error.empty(); }, 30000))
        return "a file: base was not refused with an error";
    usleep(800 * 1000);
    if (facts(v).title != "Blank")
        return "a file: base loaded: " + describeFull(facts(v));
    note("FULL:load_html:about:blank shown, file: refused:%s", facts(v).error.c_str());
    destroy(v);
    if (facts(v).violations)
        return "the engine told something about the id after view_destroy";
    return "";
}

std::string modeProfiles()
{
    const std::string marker = std::to_string(getpid()) + "x" + std::to_string(monotonicMs());
    const std::string headerValue = "alpha" + marker, scriptValue = "beta" + marker, memoryValue = "gamma" + marker;
    Server server([&](const Request& r) {
        if (r.path == "/set") {
            Response response = html("<!doctype html><title>?</title><script>document.cookie='dc=" + scriptValue
                + "; path=/'; document.title='set:' + document.cookie</script>");
            response.headers.push_back("Set-Cookie: sc=" + headerValue + "; Path=/");
            return response;
        }
        if (r.path == "/setm") {
            Response response = html("<!doctype html><title>setm</title>");
            response.headers.push_back("Set-Cookie: mc=" + memoryValue + "; Path=/");
            return response;
        }
        if (r.path == "/check")
            return html("<!doctype html><title>check</title>");
        return missing();
    });
    if (!server.start())
        return "the server did not start";
    const std::string origin = server.origin();
    setAnswers(Answers {});
    const std::string a = scratchFolder("profile-a") + "/profile", b = scratchFolder("profile-b") + "/profile";
    auto make = [](const std::string& profile, int memoryOnly) {
        FullOptions options;
        options.profile = profile;
        options.memoryOnly = memoryOnly;
        return createFull(options);
    };
    // What the view's request for /check carried.
    auto check = [&](uint64_t view, std::string& cookie) -> std::string {
        const int before = server.count("/check");
        const int finished = facts(view).finished;
        webd_engine_load(view, (origin + "/check").c_str());
        if (!waitView(view, [&](const ViewFacts& f) { return f.finished > finished && f.title == "check" && !f.loading; }, 60000) || server.count("/check") <= before)
            return "the check page did not load: " + describeFull(facts(view));
        bool found = false;
        cookie = server.last("/check", found).cookie;
        return "";
    };

    const uint64_t a1 = make(a, 0);
    webd_engine_load(a1, (origin + "/set").c_str());
    if (!waitView(a1, [&](const ViewFacts& f) { return f.title.find("dc=" + scriptValue) != std::string::npos && !f.loading; }, 60000))
        return "profile A's page did not set its cookies: " + describeFull(facts(a1));
    std::string cookie, why;
    // A second view of A (a process of its own: no group) sees both cookies.
    const uint64_t a2 = make(a, 0);
    if (!(why = check(a2, cookie)).empty())
        return why;
    note("PROFILES:A again:%s", cookie.c_str());
    if (cookie.find("sc=" + headerValue) == std::string::npos || cookie.find("dc=" + scriptValue) == std::string::npos)
        return "a second view of profile A did not send its cookies: '" + cookie + "'";
    // Profile B and a memory-only view see nothing of A's.
    const uint64_t b1 = make(b, 0);
    if (!(why = check(b1, cookie)).empty())
        return why;
    note("PROFILES:B:'%s'", cookie.c_str());
    if (!cookie.empty())
        return "profile B sent a cookie: '" + cookie + "'";
    const uint64_t m1 = make("", 1);
    if (!(why = check(m1, cookie)).empty())
        return why;
    if (!cookie.empty())
        return "a memory-only view sent a cookie: '" + cookie + "'";
    // ... and A sees nothing of the memory-only view's.
    const int finished = facts(m1).finished;
    webd_engine_load(m1, (origin + "/setm").c_str());
    if (!waitView(m1, [&](const ViewFacts& f) { return f.finished > finished && f.title == "setm"; }, 60000))
        return "the memory-only view's page did not load";
    if (!(why = check(a2, cookie)).empty())
        return why;
    if (cookie.find("mc=") != std::string::npos)
        return "profile A sent the memory-only view's cookie: '" + cookie + "'";

    // The children's environment is the one the host started with, after every kind of profile was made.
    const char* jar = getenv("CURL_COOKIE_JAR_PATH");
    const char* children = getenv("LAZYOS_WEBKIT_PROFILE");
    note("PROFILES:environment:CURL_COOKIE_JAR_PATH=%s:LAZYOS_WEBKIT_PROFILE=%s", jar ? jar : "(unset)", children ? children : "(unset)");
    if (!jar || strcmp(jar, ":memory:") || !children || childrenDirectory != children)
        return "the children's environment changed after the host started";
    // The cookies are in A's jar, and in no other file the engine processes wrote.
    std::string where;
    bool inA = false;
    for (int waited = 0; waited < 100 && !(inA = folderHolds(a + "/cookies", headerValue, where)); ++waited)
        usleep(100 * 1000);
    if (!inA)
        return "profile A's cookie is not in a file below " + a + "/cookies";
    note("PROFILES:A's jar:%s", where.c_str());
    if (folderHolds(b, headerValue, where) || folderHolds(b, scriptValue, where))
        return "profile A's cookie is in " + where;
    if (folderHolds(childrenDirectory, headerValue, where) || folderHolds(".", headerValue, where))
        return "profile A's cookie is in " + where + " (a default jar)";
    if (folderHolds(a, memoryValue, where) || folderHolds(childrenDirectory, memoryValue, where) || folderHolds(".", memoryValue, where))
        return "the memory-only view's cookie was written to " + where;
    note("PROFILES:B holds nothing of A's; the memory-only cookie is on no disk");
    for (uint64_t id : { a1, a2, b1, m1 })
        destroy(id);
    return "";
}

std::string modeAsk()
{
    Server other([](const Request& r) { return r.path == "/b" ? html(linkPage("B", "")) : missing(); });
    if (!other.start())
        return "the second server did not start";
    const std::string elsewhere = other.origin();
    Server home([&](const Request& r) {
        if (r.path == "/a")
            return html(linkPage("A", elsewhere + "/b"));
        if (r.path == "/a2")
            return html(linkPage("A2", ""));
        return missing();
    });
    if (!home.start())
        return "the server did not start";
    const std::string origin = home.origin();
    // A link to the other origin is held; everything else goes.
    setAnswers(Answers { [&](uint64_t, const Navigation& n) {
        return n.url.compare(0, elsewhere.size(), elsewhere) == 0 && n.kind == 0 ? WEBD_VERDICT_ASK : WEBD_VERDICT_ALLOW;
    }, nullptr, "" });
    FullOptions options;
    options.features = WEBD_FEATURE_POLICY | WEBD_FEATURE_PAGE_UI;
    const uint64_t v = createFull(options);
    auto heldClick = [&](uint64_t& request) -> std::string {
        const size_t before = facts(v).navigations.size();
        click(v, 50, 50);
        if (!waitView(v, [&](const ViewFacts& f) { return f.navigations.size() > before && f.navigations.back().verdict == WEBD_VERDICT_ASK; }, 30000))
            return "the click was not asked about: " + describeFull(facts(v));
        const Navigation n = facts(v).navigations.back();
        if (!n.gesture || !n.mainFrame || n.url != elsewhere + "/b")
            return "the held navigation is not a main-frame click to the other origin";
        request = n.request;
        return "";
    };
    auto stays = [&](const char* title, int requests) -> std::string {
        usleep(1500 * 1000);
        const ViewFacts f = facts(v);
        if (other.count("/b") != requests || f.title != title || f.loading)
            return std::string("a held or refused navigation loaded: ") + other.describe() + " " + describeFull(f);
        return "";
    };

    webd_engine_load(v, (origin + "/a").c_str());
    if (!waitPage(v, "A"))
        return "the first page did not load: " + describeFull(facts(v));
    uint64_t first = 0, second = 0, third = 0;
    std::string why = heldClick(first);
    if (!why.empty() || !(why = stays("A", 0)).empty())
        return why;
    note("ASK:held %llu: nothing loaded", static_cast<unsigned long long>(first));
    webd_engine_navigation_answer(v, first, 1);
    if (!waitPage(v, "B") || other.count("/b") != 1)
        return "an allowed held navigation did not load: " + describeFull(facts(v));
    // Answered: a second answer is nothing.
    webd_engine_navigation_answer(v, first, 1);
    note("ASK:allowed %llu: loaded", static_cast<unsigned long long>(first));

    webd_engine_back(v);
    if (!waitView(v, [](const ViewFacts& f) { return f.title == "A" && !f.loading && f.canForward; }, 60000))
        return "Back did not return to the first page: " + describeFull(facts(v));
    if (!(why = heldClick(second)).empty())
        return why;
    webd_engine_navigation_answer(v, second, 0);
    if (!(why = stays("A", 1)).empty())
        return why;
    note("ASK:denied %llu: stayed", static_cast<unsigned long long>(second));

    // A newer main-frame navigation drops the held one: its answer, late, is nothing.
    if (!(why = heldClick(third)).empty())
        return why;
    webd_engine_load(v, (origin + "/a2").c_str());
    if (!waitPage(v, "A2"))
        return "the newer navigation did not load: " + describeFull(facts(v));
    webd_engine_navigation_answer(v, third, 1);
    if (!(why = stays("A2", 1)).empty())
        return why;
    note("ASK:dropped %llu by a newer navigation: its answer did nothing", static_cast<unsigned long long>(third));

    // A view that ends with a navigation held ends cleanly.
    webd_engine_load(v, (origin + "/a").c_str());
    if (!waitPage(v, "A") || !(why = heldClick(first)).empty())
        return "the last held navigation was not made: " + why;
    destroy(v);
    webd_engine_navigation_answer(v, first, 1);
    usleep(800 * 1000);
    if (facts(v).violations || other.count("/b") != 1)
        return "a held navigation outlived its view";
    // Ids are per view and never reused.
    const ViewFacts f = facts(v);
    for (size_t i = 1; i < f.navigations.size(); ++i) {
        if (f.navigations[i].request <= f.navigations[i - 1].request)
            return "navigation request ids were reused";
    }
    return "";
}

} // namespace webdtest
