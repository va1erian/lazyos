// html, script, config and lock: one or two views each (support.h).

#include "support.h"

#include <algorithm>
#include <stdio.h>
#include <unistd.h>

namespace webdtest {

namespace {

std::string describe(const ViewFacts& f)
{
    char text[400];
    snprintf(text, sizeof text, "title='%s' finished=%d loading=%d error='%s' frames=%d last=%c seen=%s links=%zu lost=%d", f.title.c_str(),
        f.finished, f.loading, f.error.c_str(), f.frames, f.last, f.seen.substr(0, 24).c_str(), f.links.size(), f.lostReason);
    return text;
}

bool has(const ViewFacts& f, const std::string& url)
{
    return std::find(f.links.begin(), f.links.end(), url) != f.links.end();
}

} // namespace

std::string modeHtml()
{
    Options options;
    options.width = 320;
    options.height = 200;
    const uint64_t v = create(options);
    // Success is the first state for the id.
    if (!waitView(v, [](const ViewFacts& f) { return f.states >= 1; }, 60000))
        return "no state after view_create";
    loadHtml(v, page("Hi", "#f00"));
    if (!waitView(v, [](const ViewFacts& f) { return shows(f, "Hi", 'r', 1); }, 60000))
        return "the red page did not show: " + describe(facts(v));
    ViewFacts f = facts(v);
    note("PAGE:title=%s:colour=red:size=%dx%d:frames=%d:scrolls=%d", f.title.c_str(), f.width, f.height, f.frames, f.scrolls);
    if (f.width != 320 || f.height != 200)
        return "the frame is not the view's size: " + describe(f);
    if (f.scrolls < 1)
        return "no scroll offset was reported with the frames";

    loadHtml(v, page("Two", "#0a0"));
    if (!waitView(v, [](const ViewFacts& g) { return shows(g, "Two", 'g', 2); }, 30000))
        return "the green page did not replace the red one: " + describe(facts(v));
    note("PAGE:title=Two:colour=green:finished=%d", facts(v).finished);

    webd_engine_resize(v, 400, 300);
    if (!waitView(v, [](const ViewFacts& g) { return g.width == 400 && g.height == 300; }, 30000))
        return "no 400x300 frame after resize: " + describe(facts(v));
    note("RESIZE:400x300");

    // Not visible: the page runs (its title and load state arrive), no frame does until it is shown again.
    webd_engine_set_visible(v, 0);
    usleep(500 * 1000);
    const int frozen = facts(v).frames;
    loadHtml(v, page("Three", "#00f"));
    if (!waitView(v, [](const ViewFacts& g) { return g.finished >= 3 && g.title == "Three"; }, 30000))
        return "a hidden view's page did not run: " + describe(facts(v));
    usleep(1500 * 1000);
    if (facts(v).frames != frozen)
        return "a hidden view was painted";
    webd_engine_set_visible(v, 1);
    if (!waitView(v, [&](const ViewFacts& g) { return g.last == 'b' && g.frames > frozen; }, 30000))
        return "no frame when the view was shown again: " + describe(facts(v));
    note("VISIBLE:hidden frames=0:shown=%d", facts(v).frames - frozen);

    const int before = facts(v).frames;
    webd_engine_refresh(v);
    if (!waitView(v, [&](const ViewFacts& g) { return g.frames > before; }, 30000))
        return "no whole frame after refresh";
    note("REFRESH:ok");

    f = facts(v);
    if (f.lost || !f.error.empty())
        return "the view failed: " + describe(f);
    destroy(v);
    f = facts(v);
    if (f.violations)
        return "the engine told something about the id after view_destroy: " + describe(f);
    // Calls about a destroyed id are ignored, whatever they are.
    loadHtml(v, page("Ghost", "#f00"));
    webd_engine_resize(v, 10, 10);
    webd_engine_mouse(v, 1, 0, 1, 1, 0);
    webd_engine_set_visible(v, 1);
    usleep(500 * 1000);
    if (facts(v).violations)
        return "a call about a destroyed id was answered";
    return "";
}

std::string modeScript()
{
    // Two views at once: the page's script writes the title, and <noscript> turns the page red.
    const std::string html = page("t", "#00f",
        "<noscript><style>body{background:#f00}</style></noscript><script>document.title='x'</script>");
    Options off, on;
    on.scripts = 1;
    const uint64_t a = create(off), b = create(on);
    loadHtml(a, html);
    loadHtml(b, html);
    if (!waitView(b, [](const ViewFacts& f) { return f.title == "x" && f.last == 'b'; }, 60000))
        return "scripts=1: the script did not run: " + describe(facts(b));
    if (!waitView(a, [](const ViewFacts& f) { return f.finished >= 1; }, 60000))
        return "scripts=0: the page did not load: " + describe(facts(a));
    usleep(1000 * 1000);
    const ViewFacts fa = facts(a), fb = facts(b);
    // What the parser made of <noscript> (red if it was rendered) is WebKit's business; the script's is ours.
    note("SCRIPT:scripts=0:title=%s:colour=%c", fa.title.c_str(), fa.last);
    note("SCRIPT:scripts=1:title=%s:colour=%c", fb.title.c_str(), fb.last);
    if (fa.title != "t")
        return "scripts=0: the script ran: " + describe(fa);
    if (fb.title != "x")
        return "scripts=1: the script stopped: " + describe(fb);
    // A javascript: link and an event handler are script too.
    const std::string events = page("e", "#00f", "<img src=\"data:image/gif;base64,R0lGODlhAQABAAAAACw=\" onload=\"document.title='h'\">");
    loadHtml(a, events);
    if (!waitView(a, [](const ViewFacts& f) { return f.title == "e" && f.finished >= 2; }, 30000))
        return "scripts=0: the second page did not load: " + describe(facts(a));
    usleep(800 * 1000);
    if (facts(a).title != "e")
        return "scripts=0: an event handler ran";
    destroy(a);
    destroy(b);
    return "";
}

std::string modeConfig()
{
    // Best effort fields: each is applied, or the engine said CONFIG:IGNORED:<field>.
    Options dark, light, font, styled;
    dark.scripts = light.scripts = font.scripts = 1;
    dark.colorScheme = 1;
    font.fontSize = 24;
    styled.style = "html, body { background: #0a0 !important }";
    const std::string probe = page("?", "#00f",
        "<script>document.title = (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light') + ' ' + "
        "getComputedStyle(document.body).fontSize</script>");
    const uint64_t d = create(dark), l = create(light), f = create(font), s = create(styled);
    loadHtml(d, probe);
    loadHtml(l, probe);
    loadHtml(f, probe);
    loadHtml(s, page("S", "#f00"));
    std::string result;
    auto settled = [](const ViewFacts& g) { return g.finished >= 1 && g.title != "" && g.title != "?"; };
    if (!waitView(d, settled, 60000) || !waitView(l, settled, 60000) || !waitView(f, settled, 60000))
        return "a probe page did not load";
    if (!waitView(s, [](const ViewFacts& g) { return g.finished >= 1 && g.last != '?'; }, 60000))
        return "the styled page did not paint";
    usleep(500 * 1000);
    auto report = [&](const char* field, bool applied) {
        const bool ignored = said(std::string("CONFIG:IGNORED:") + field);
        note("CONFIG:%s:%s", field, applied ? "APPLIED" : ignored ? "IGNORED" : "MISSING");
        if (!applied && !ignored)
            result += std::string(" ") + field + " neither applied nor reported;";
    };
    // prefers-color-scheme is compiled out of this WebKit (ENABLE_DARK_MODE_CSS): the engine must say so.
    report("color_scheme", facts(d).title.compare(0, 4, "dark") == 0 && facts(l).title.compare(0, 5, "light") == 0);
    report("default_font_size", facts(f).title.find("24px") != std::string::npos);
    report("user_style", facts(s).last == 'g');
    note("CONFIG:titles:dark='%s':light='%s':font='%s':style colour=%c", facts(d).title.c_str(), facts(l).title.c_str(),
        facts(f).title.c_str(), facts(s).last);
    for (uint64_t id : { d, l, f, s })
        destroy(id);
    return result;
}

std::string modeLock()
{
    // A page that links, submits and refreshes elsewhere. None of it loads; each is told as a link.
    const std::string body =
        "<meta http-equiv=\"refresh\" content=\"1;url=http://example.com/refresh\">"
        "<a href=\"https://example.com/link\" style=\"display:block;position:absolute;left:0;top:0;width:200px;height:100px;background:#f00\">link</a>"
        "<form action=\"http://example.com/submit\" method=\"get\"><input type=\"hidden\" name=\"a\" value=\"1\">"
        "<input type=\"submit\" style=\"position:absolute;left:0;top:150px;width:200px;height:80px\"></form>"
        "<a href=\"#frag\" style=\"display:block;position:absolute;left:300px;top:0;width:80px;height:40px;background:#00f\">in</a>";
    Options options;
    options.width = 400;
    options.height = 300;
    const uint64_t v = create(options);
    loadHtml(v, "<!doctype html><title>Lock</title><body style=\"margin:0;background:#fff\">" + body);
    if (!waitView(v, [](const ViewFacts& f) { return f.finished >= 1 && f.last == 'w'; }, 60000))
        return "the page did not show: " + describe(facts(v));
    auto click = [&](int x, int y) {
        webd_engine_mouse(v, 0, 0, x, y, 0);
        webd_engine_mouse(v, 1, 0, x, y, 0);
        webd_engine_mouse(v, 2, 0, x, y, 0);
    };
    // The refresh fires on its own a second after the load. (A navigation the page starts, even one that is
    // refused, cancels the one that is scheduled: the clicks wait for it.)
    if (!waitView(v, [](const ViewFacts& f) { return has(f, "http://example.com/refresh"); }, 30000))
        return "the meta refresh was not reported: " + describe(facts(v));
    click(50, 50);
    if (!waitView(v, [](const ViewFacts& f) { return has(f, "https://example.com/link"); }, 30000))
        return "the link click was not reported: " + describe(facts(v));
    click(50, 190);
    if (!waitView(v, [](const ViewFacts& f) { return std::any_of(f.links.begin(), f.links.end(),
                                                  [](const std::string& u) { return u.compare(0, 27, "http://example.com/submit?a") == 0; }); }, 30000))
        return "the form submit was not reported: " + describe(facts(v));
    // An in-page #fragment is the page's own: nothing reported, nothing loaded.
    const size_t before = facts(v).links.size();
    click(340, 20);
    usleep(1500 * 1000);
    const ViewFacts f = facts(v);
    for (const std::string& url : f.links)
        note("LINK:%s", url.c_str());
    if (f.links.size() != before)
        return "an in-page link was reported as a navigation";
    if (f.finished != 1 || f.title != "Lock" || f.last != 'w' || !f.error.empty() || f.lost)
        return "the page did not stay: " + describe(f);

    // Script that navigates is a navigation too (and a window it opens is not a window).
    Options scripted;
    scripted.scripts = 1;
    const uint64_t s = create(scripted);
    loadHtml(s, page("Script", "#fff", "<script>setTimeout(function(){location.href='http://example.com/script'},200)</script>"));
    if (!waitView(s, [](const ViewFacts& g) { return has(g, "http://example.com/script"); }, 30000))
        return "a script's location change was not reported: " + describe(facts(s));
    usleep(1000 * 1000);
    const ViewFacts g = facts(s);
    if (g.finished != 1 || g.title != "Script" || !g.error.empty())
        return "the scripted page did not stay: " + describe(g);
    note("LOCK:links=%zu:script links=%zu:finished=%d", f.links.size(), g.links.size(), g.finished);
    destroy(v);
    destroy(s);
    return "";
}

} // namespace webdtest
