// dialog, download and popup: what a full view's page asks of the service (JavaScript dialogs, a file to save,
// a new window), each answered the way webd_abi.h says.

#include "support.h"

#include <fstream>
#include <iterator>
#include <stdio.h>
#include <unistd.h>

#include "server.h"

namespace webdtest {

namespace {

Response html(const std::string& body) { return Response { 200, "text/html; charset=utf-8", {}, body }; }
Response missing() { return Response { 404, "text/plain", {}, "missing" }; }

// Waits for dialog number `index` (from 0) of the view and returns it (id 0: none came).
Dialog nextDialog(uint64_t view, size_t index, int ms = 30000)
{
    if (!waitView(view, [&](const ViewFacts& f) { return f.dialogs.size() > index; }, ms))
        return Dialog {};
    return facts(view).dialogs[index];
}

std::string fileBytes(const std::string& path)
{
    std::ifstream file(path, std::ios::binary);
    return std::string((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
}

} // namespace

std::string modeDialog()
{
    Server server([](const Request& r) {
        if (r.path == "/ask")
            return html("<!doctype html><title>ask</title><script>alert('hello'); var c = confirm('sure?'); var p = prompt('name?', 'def');"
                        " document.title = 'done:' + c + ':' + p</script>");
        if (r.path == "/no")
            return html("<!doctype html><title>no</title><script>var c = confirm('x'); var p = prompt('y', 'z'); document.title = c + ':' + p</script>");
        if (r.path == "/loop")
            return html("<!doctype html><title>loop</title><script>for (var i = 0; i < 25; i++) alert('L' + i); document.title = 'looped'</script>");
        if (r.path == "/forever")
            return html("<!doctype html><title>forever</title><script>for (var i = 0;; i++) alert('F' + i)</script>");
        return missing();
    });
    if (!server.start())
        return "the server did not start";
    setAnswers(Answers {});
    FullOptions options;
    options.features = WEBD_FEATURE_POLICY | WEBD_FEATURE_DIALOGS;
    const uint64_t v = createFull(options);
    webd_engine_load(v, (server.origin() + "/ask").c_str());
    Dialog d = nextDialog(v, 0);
    if (!d.id || d.kind != 0 || d.message != "hello")
        return "no alert: " + describeFull(facts(v));
    webd_engine_dialog_reply(v, d.id, 1, nullptr);
    d = nextDialog(v, 1);
    if (!d.id || d.kind != 1 || d.message != "sure?")
        return "no confirm after the alert was answered";
    webd_engine_dialog_reply(v, d.id, 1, nullptr);
    d = nextDialog(v, 2);
    if (!d.id || d.kind != 2 || d.message != "name?" || d.defaultValue != "def")
        return "no prompt (or not its default) after the confirm was answered";
    webd_engine_dialog_reply(v, d.id, 1, "typed");
    // A reply to a dialog already answered is nothing.
    webd_engine_dialog_reply(v, d.id, 1, "again");
    if (!waitView(v, [](const ViewFacts& f) { return f.title == "done:true:typed"; }, 30000))
        return "the page did not get the answers: " + describeFull(facts(v));
    note("DIALOG:alert, confirm, prompt answered:%s", facts(v).title.c_str());

    webd_engine_load(v, (server.origin() + "/no").c_str());
    for (size_t i = 3; i < 5; ++i) {
        d = nextDialog(v, i);
        if (!d.id)
            return "the cancelled dialogs did not come";
        webd_engine_dialog_reply(v, d.id, 0, "ignored");
    }
    if (!waitView(v, [](const ViewFacts& f) { return f.title == "false:null"; }, 30000))
        return "Cancel was not the page's answer: " + describeFull(facts(v));

    // A page that loops alerts is answered every time, in order.
    webd_engine_load(v, (server.origin() + "/loop").c_str());
    const size_t start = facts(v).dialogs.size();
    for (size_t i = 0; i < 25; ++i) {
        d = nextDialog(v, start + i);
        if (!d.id || d.message != "L" + std::to_string(i))
            return "alert " + std::to_string(i) + " of the loop did not come: " + describeFull(facts(v));
        webd_engine_dialog_reply(v, d.id, 1, nullptr);
    }
    if (!waitView(v, [](const ViewFacts& f) { return f.title == "looped"; }, 30000))
        return "the looping page did not end: " + describeFull(facts(v));
    note("DIALOG:25 alerts of a loop answered");

    // One that never stops: answered for a while, then its view ends with a dialog open.
    webd_engine_load(v, (server.origin() + "/forever").c_str());
    const size_t from = facts(v).dialogs.size();
    for (size_t i = 0; i < 10; ++i) {
        d = nextDialog(v, from + i);
        if (!d.id)
            return "the endless page's alert " + std::to_string(i) + " did not come";
        webd_engine_dialog_reply(v, d.id, 1, nullptr);
    }
    if (!nextDialog(v, from + 10).id)
        return "the endless page stopped asking";
    destroy(v);
    if (facts(v).violations)
        return "the engine told something about the id after view_destroy";
    // The host goes on.
    const uint64_t again = createFull(options);
    webd_engine_load(again, (server.origin() + "/no").c_str());
    if (!nextDialog(again, 0).id)
        return "a view made after one ended in a dialog got no dialog";
    destroy(again);
    return "";
}

std::string modeDownload()
{
    std::string data;
    for (int i = 0; i < 300 * 1024; ++i)
        data += static_cast<char>((i * 7 + i / 251) & 0xff);
    const std::string slow(4 * 1024 * 1024, 's');
    Server server([&](const Request& r) {
        if (r.path == "/page")
            return html(linkPage("Page", "/file.bin"));
        if (r.path == "/file.bin")
            return Response { 200, "application/octet-stream", { "Content-Disposition: attachment; filename=\"data.bin\"" }, data };
        if (r.path == "/slow.bin")
            return Response { 200, "application/octet-stream", { "Content-Disposition: attachment; filename=\"slow.bin\"" }, slow, 16 * 1024, 50 };
        return missing();
    });
    if (!server.start())
        return "the server did not start";
    const std::string folder = scratchFolder("downloads");
    setAnswers(Answers { nullptr, nullptr, folder });
    FullOptions options;
    options.features = WEBD_FEATURE_POLICY | WEBD_FEATURE_DOWNLOADS;
    const uint64_t v = createFull(options);
    webd_engine_load(v, (server.origin() + "/page").c_str());
    if (!waitPage(v, "Page"))
        return "the page did not load: " + describeFull(facts(v));
    click(v, 50, 50);
    if (!waitView(v, [](const ViewFacts& f) { return f.downloads.count(1) && (f.downloads.at(1).done || f.downloads.at(1).failed); }, 60000))
        return "the attachment did not download: " + describeFull(facts(v));
    ViewFacts f = facts(v);
    const Download done = f.downloads.at(1);
    note("DOWNLOAD:1:name=%s:received=%lld:total=%lld:progress=%d:path=%s", done.name.c_str(), static_cast<long long>(done.received),
        static_cast<long long>(done.total), done.progress, done.path.c_str());
    if (done.failed || done.name != "data.bin" || done.progress < 1 || done.received != static_cast<int64_t>(data.size())
        || done.total != static_cast<int64_t>(data.size()))
        return "the download's events are wrong (" + done.reason + ")";
    if (fileBytes(done.path) != data)
        return "the downloaded file does not hold the server's bytes";
    if (f.title != "Page" || f.finished != 1 || f.committed != server.origin() + "/page")
        return "the page did not stay when its link became a download: " + describeFull(f);

    // Save As of a slow file, cancelled half way.
    webd_engine_download(v, (server.origin() + "/slow.bin").c_str());
    if (!waitView(v, [](const ViewFacts& g) { return g.downloads.count(2) && g.downloads.at(2).progress >= 1; }, 60000))
        return "the slow download did not start: " + describeFull(facts(v));
    webd_engine_download_cancel(v, 2);
    if (!waitView(v, [](const ViewFacts& g) { return g.downloads.at(2).failed || g.downloads.at(2).done; }, 60000))
        return "the cancelled download did not end";
    const Download cancelled = facts(v).downloads.at(2);
    note("DOWNLOAD:2:%s:received=%lld of %zu:reason=%s", cancelled.failed ? "failed" : "done", static_cast<long long>(cancelled.received),
        slow.size(), cancelled.reason.c_str());
    if (!cancelled.failed || cancelled.reason.empty() || cancelled.name != "slow.bin")
        return "a cancelled download did not fail with a reason";
    // A cancel of a download that ended is nothing.
    webd_engine_download_cancel(v, 1);
    webd_engine_download_cancel(v, 99);
    usleep(500 * 1000);
    if (facts(v).lost || facts(v).title != "Page")
        return "the view did not stay after the downloads";
    destroy(v);
    return "";
}

std::string modePopup()
{
    Server server([](const Request& r) {
        if (r.path == "/p")
            return html(linkPage("P", "", "<button onclick=\"window.open('/w1')\" style=\"position:absolute;left:0;top:0;width:200px;height:100px\">open</button>"
                                                 "<script>setTimeout(function() { window.open('/w2'); document.title = 'opened' }, 300)</script>"));
        if (r.path == "/t")
            return html(linkPage("T", "", "<a href=\"/w3\" target=\"_blank\" style=\"display:block;position:absolute;left:0;top:0;width:200px;height:100px\">t</a>"));
        return html(linkPage("W", ""));
    });
    if (!server.start())
        return "the server did not start";
    setAnswers(Answers {});
    FullOptions options;
    options.features = WEBD_FEATURE_POLICY | WEBD_FEATURE_PAGE_UI;
    const uint64_t v = createFull(options);
    const std::string origin = server.origin();
    webd_engine_load(v, (origin + "/p").c_str());
    // A script's window.open, with no gesture.
    if (!waitView(v, [](const ViewFacts& f) { return f.title == "opened" && !f.newWindows.empty(); }, 60000))
        return "the script's window.open was not told: " + describeFull(facts(v));
    ViewFacts f = facts(v);
    if (f.newWindows[0].first != origin + "/w2" || f.newWindows[0].second != 0)
        return "the script's window was told as '" + f.newWindows[0].first + "' gesture " + std::to_string(f.newWindows[0].second);
    // A click's window.open, with one.
    click(v, 50, 50);
    if (!waitView(v, [](const ViewFacts& g) { return g.newWindows.size() >= 2; }, 30000))
        return "the click's window.open was not told";
    f = facts(v);
    if (f.newWindows[1].first != origin + "/w1" || f.newWindows[1].second != 1)
        return "the click's window was told as '" + f.newWindows[1].first + "' gesture " + std::to_string(f.newWindows[1].second);
    // A target=_blank link is a new window too.
    webd_engine_load(v, (origin + "/t").c_str());
    if (!waitPage(v, "T"))
        return "the second page did not load";
    click(v, 50, 50);
    if (!waitView(v, [](const ViewFacts& g) { return g.newWindows.size() >= 3; }, 30000))
        return "the target=_blank link was not told";
    f = facts(v);
    usleep(800 * 1000);
    note("POPUP:script=%s/%d:click=%s/%d:blank=%s/%d", f.newWindows[0].first.c_str(), f.newWindows[0].second, f.newWindows[1].first.c_str(),
        f.newWindows[1].second, f.newWindows[2].first.c_str(), f.newWindows[2].second);
    if (f.newWindows[2].first != origin + "/w3" || f.newWindows[2].second != 1)
        return "the target=_blank window was told wrong";
    // The link's new window was asked about first, as a link that replaces a page (main_frame) with its gesture.
    // window.open is not: WebKit asks for the window (webd_ui_new_window) without a navigation policy.
    bool asked = false;
    for (const Navigation& n : f.navigations) {
        note("POPUP:asked:%s:kind=%d:gesture=%d:main=%d", n.url.c_str(), n.kind, n.gesture, n.mainFrame);
        asked = asked || (n.url == origin + "/w3" && n.kind == 0 && n.gesture && n.mainFrame);
        if (n.url == origin + "/w1" || n.url == origin + "/w2")
            return "a window.open was asked about as a navigation";
    }
    if (!asked)
        return "the target=_blank link was not asked about as a main-frame link with its gesture";
    // Nothing opened: no page of the windows was asked for, and the view stayed on its page.
    if (server.count("/w") != 0 || facts(v).title != "T")
        return "a new window loaded something: " + server.describe();
    destroy(v);
    return "";
}

std::string modeNotify()
{
    Server server([](const Request&) {
        return html(linkPage("pending", "",
            "<button onclick=\"act()\" style=\"position:absolute;left:0;top:0;width:200px;height:100px\">go</button><script>"
            "function act() { if (Notification.permission === 'granted') { var n = new Notification('Hello', { body: 'World', tag: 't1' });"
            " n.onshow = function() { document.title = 'shown' }; n.onclick = function() { document.title = 'clicked' } }"
            " else Notification.requestPermission().then(function(p) { document.title = p }) }"
            "if (!window.isSecureContext) document.title = 'insecure'</script>"));
    });
    if (!server.start())
        return "the server did not start";
    setAnswers(Answers {});
    FullOptions options;
    options.features = WEBD_FEATURE_POLICY | WEBD_FEATURE_NOTIFICATIONS;
    // A persistent profile: WebKit refuses the permission in a page whose session is ephemeral
    // (WebNotificationClient::requestPermission), before anything reaches the UI process.
    options.profile = scratchFolder("notify") + "/profile";
    // Two views of one origin, each a client of its own: one answer must not settle the other's question.
    const uint64_t a = createFull(options), b = createFull(options);
    const std::string origin = server.origin();
    for (uint64_t v : { a, b }) {
        webd_engine_load(v, (origin + "/n").c_str());
        if (!waitPage(v, "pending"))
            return "the page did not load: " + describeFull(facts(v));
        click(v, 50, 50);
        if (!waitView(v, [](const ViewFacts& f) { return !f.permissionRequests.empty(); }, 30000))
            return "no permission request after a click: " + describeFull(facts(v));
    }
    if (facts(a).permissionRequests[0] != origin)
        return "the request names '" + facts(a).permissionRequests[0] + "', not the page's origin";
    webd_engine_notify_permission(a, origin.c_str(), 1);
    if (!waitView(a, [](const ViewFacts& f) { return f.title == "granted"; }, 30000))
        return "the allowed view's page did not hear it: " + describeFull(facts(a));
    usleep(1000 * 1000);
    if (facts(b).title != "pending")
        return "the other view's request was settled too: " + describeFull(facts(b));
    webd_engine_notify_permission(b, origin.c_str(), -1);
    if (!waitView(b, [](const ViewFacts& f) { return f.title == "denied"; }, 30000))
        return "the dismissed request was not answered: " + describeFull(facts(b));
    note("NOTIFY:one view allowed, the other's request still pending, then dismissed");

    click(a, 50, 50);
    if (!waitView(a, [](const ViewFacts& f) { return !f.notifications.empty(); }, 30000))
        return "the allowed page's notification was not told";
    const ViewFacts f = facts(a);
    if (f.notifications[0] != std::vector<std::string> { origin, "Hello", "World" })
        return "the notification was told wrong";
    const uint64_t id = f.notificationIds[0];
    webd_engine_notify_shown(id);
    if (!waitView(a, [](const ViewFacts& g) { return g.title == "shown"; }, 30000))
        return "the page did not hear its notification was shown";
    webd_engine_notify_clicked(id);
    if (!waitView(a, [](const ViewFacts& g) { return g.title == "clicked"; }, 30000))
        return "the page did not hear its notification was clicked";
    webd_engine_notify_closed(id);
    if (!facts(b).notifications.empty())
        return "the other view was told of a notification";
    note("NOTIFY:shown, clicked and closed:id=%llu", static_cast<unsigned long long>(id));
    destroy(a);
    destroy(b);
    return "";
}

} // namespace webdtest
