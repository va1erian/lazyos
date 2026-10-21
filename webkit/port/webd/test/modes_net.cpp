// network: a light view must not reach a network by any road. A listener on 127.0.0.1 counts the
// connections it is offered; a plain (browser) view proves the listener sees a load, then light views
// with pages that ask for everything they can must leave the count where it was.

#include "support.h"

#include <arpa/inet.h>
#include <atomic>
#include <mutex>
#include <netinet/in.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#include "engine.h"

namespace webdtest {

namespace {

class Listener {
public:
    ~Listener()
    {
        m_stop = true;
        if (m_started)
            pthread_join(m_thread, nullptr);
        if (m_fd >= 0)
            close(m_fd);
    }

    bool start()
    {
        m_fd = socket(AF_INET, SOCK_STREAM, 0);
        sockaddr_in address = { };
        address.sin_family = AF_INET;
        address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        socklen_t length = sizeof address;
        if (m_fd < 0 || bind(m_fd, reinterpret_cast<sockaddr*>(&address), sizeof address) != 0 || listen(m_fd, 64) != 0
            || getsockname(m_fd, reinterpret_cast<sockaddr*>(&address), &length) != 0)
            return false;
        m_port = ntohs(address.sin_port);
        m_started = pthread_create(&m_thread, nullptr, &Listener::serve, this) == 0;
        return m_started;
    }

    int port() const { return m_port; }
    int accepted() const { return m_accepted; }
    std::string requests()
    {
        std::lock_guard<std::mutex> hold(m_mutex);
        std::string all;
        for (const std::string& line : m_requests)
            all += (all.empty() ? "" : " | ") + line;
        return all;
    }

private:
    static void* serve(void* self) { return static_cast<Listener*>(self)->run(); }

    void* run()
    {
        while (!m_stop) {
            pollfd waiting = { m_fd, POLLIN, 0 };
            if (poll(&waiting, 1, 100) <= 0)
                continue;
            const int client = accept(m_fd, nullptr, nullptr);
            if (client < 0)
                continue;
            ++m_accepted;
            timeval timeout = { 0, 200 * 1000 };
            setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof timeout);
            char text[160] = { 0 };
            const ssize_t got = recv(client, text, sizeof text - 1, 0);
            if (char* end = strpbrk(text, "\r\n"))
                *end = 0;
            {
                std::lock_guard<std::mutex> hold(m_mutex);
                m_requests.push_back(got > 0 ? text : "(connect only)");
            }
            close(client);
        }
        return nullptr;
    }

    int m_fd { -1 };
    int m_port { 0 };
    std::atomic<int> m_accepted { 0 };
    std::atomic<bool> m_stop { false };
    bool m_started { false };
    pthread_t m_thread { };
    std::mutex m_mutex;
    std::vector<std::string> m_requests;
};

// A browser view (no lock) made by the engine directly, to show the listener sees what a view loads.
struct Control {
    int port { 0 };
    int engineId { 0 };
};

class QuietSink final : public webkitapp::ViewSink {
};

void makeControl(void* context)
{
    auto* control = static_cast<Control*>(context);
    static QuietSink sink;
    webkitapp::ViewConfig config;
    config.width = 100;
    config.height = 100;
    webkitapp::View* view = webkitapp::createView(config, &sink);
    if (!view)
        return;
    control->engineId = view->id;
    view->loadHtml("<img src=\"http://127.0.0.1:" + std::to_string(control->port) + "/control\">", "about:blank");
}

void endControl(void* context) { webkitapp::destroyView(static_cast<Control*>(context)->engineId); }

// Every road a page has to a network, as markup and as script.
std::string everything(int port)
{
    const std::string base = "http://127.0.0.1:" + std::to_string(port);
    auto at = [&](const char* path) { return base + path; };
    std::string body;
    body += "<img src=\"" + at("/img.png") + "\">";
    body += "<link rel=\"stylesheet\" href=\"" + at("/a.css") + "\">";
    body += "<style>@import url(\"" + at("/import.css") + "\"); body { background-image: url(\"" + at("/bg.png") + "\") }"
            " @font-face { font-family: f; src: url(\"" + at("/font.woff") + "\") } p { font-family: f }</style>";
    body += "<p>text</p>";
    body += "<script src=\"" + at("/s.js") + "\"></script>";
    body += "<iframe src=\"" + at("/frame.html") + "\"></iframe>";
    body += "<link rel=\"preconnect\" href=\"" + base + "\">";
    body += "<link rel=\"prefetch\" href=\"" + at("/prefetch") + "\">";
    body += "<link rel=\"dns-prefetch\" href=\"" + base + "\">";
    body += "<link rel=\"icon\" href=\"" + at("/icon.png") + "\">";
    body += "<video poster=\"" + at("/poster.png") + "\"></video><audio src=\"" + at("/a.mp3") + "\" preload=\"auto\"></audio>";
    body += "<object data=\"" + at("/o") + "\"></object><embed src=\"" + at("/e") + "\">";
    body += "<input type=\"image\" src=\"" + at("/input.png") + "\"><svg width=\"9\" height=\"9\"><image href=\"" + at("/svg.png") + "\"/></svg>";
    body += "<picture><source srcset=\"" + at("/pic.png") + "\"><img alt=\"\"></picture>";
    body += "<a href=\"" + at("/ping") + "\" ping=\"" + at("/ping2") + "\">a</a>";
    body += "<script>"
            "fetch('" + at("/fetch") + "').catch(function(){});"
            "var x = new XMLHttpRequest(); x.open('GET', '" + at("/xhr") + "'); x.send();"
            "try { new WebSocket('ws://127.0.0.1:" + std::to_string(port) + "/ws') } catch (e) {}"
            "new Image().src = '" + at("/jsimg") + "';"
            "if (navigator.sendBeacon) navigator.sendBeacon('" + at("/beacon") + "', 'x');"
            "try { new EventSource('" + at("/sse") + "') } catch (e) {}"
            "try { import('" + at("/mod.js") + "').catch(function(){}) } catch (e) {}"
            "var s = document.createElement('script'); s.src = '" + at("/dyn.js") + "'; document.head.appendChild(s);"
            "var l = document.createElement('link'); l.rel = 'stylesheet'; l.href = '" + at("/dyn.css") + "'; document.head.appendChild(l);"
            "try { new Worker('" + at("/worker.js") + "') } catch (e) {}"
            "setTimeout(function() { document.title = 'done' }, 600);"
            "</script>";
    return page("N", "#fff", body);
}

} // namespace

std::string modeNetwork()
{
    Listener listener;
    if (!listener.start())
        return "the listener did not start";
    const int port = listener.port();

    // The control: a view with no lock loads from the listener, so a count of 0 below means something.
    Control control;
    control.port = port;
    webd_engine_run_on_main(makeControl, &control);
    for (int waited = 0; waited < 300 && listener.accepted() < 1; ++waited)
        usleep(100 * 1000);
    if (listener.accepted() < 1)
        return "the control view never reached the listener: the test proves nothing";
    note("CONTROL:a browser view reached the listener:%s", listener.requests().c_str());
    webd_engine_run_on_main(endControl, &control);
    usleep(1500 * 1000);
    const int baseline = listener.accepted();

    Options options;
    options.width = 200;
    options.height = 100;
    options.scripts = 1;
    const uint64_t withScript = create(options);
    options.scripts = 0;
    const uint64_t withoutScript = create(options);
    loadHtml(withScript, everything(port));
    loadHtml(withoutScript, everything(port));
    if (!waitView(withScript, [](const ViewFacts& f) { return f.title == "done"; }, 60000))
        return "scripts=1: the page did not run to its end: " + std::string(facts(withScript).title);
    if (!waitView(withoutScript, [](const ViewFacts& f) { return f.finished >= 1; }, 60000))
        return "scripts=0: the page did not load";
    // Whatever was going to be asked for has been by now.
    usleep(4000 * 1000);
    const int leaked = listener.accepted() - baseline;
    note("NETWORK:connections after the control=%d", leaked);
    if (leaked != 0)
        return "a light view reached the network (" + std::to_string(leaked) + " connections: " + listener.requests() + ")";

    // A navigation is refused too: the page's own links are told, not followed (lock mode), and a base URL
    // with a network or file scheme loads nothing at all.
    const char* const bad[] = { "http://example.com/", "https://127.0.0.1/", "file:///etc/", "ftp://example.com/", "blob:null/x" };
    for (const char* base : bad) {
        ViewFacts before = facts(withScript);
        const int states = before.states;
        loadHtml(withScript, page("Bad", "#f00"), base);
        if (!waitView(withScript, [&](const ViewFacts& f) { return f.states > states && !f.error.empty(); }, 30000))
            return std::string("a load with the base URL ") + base + " was not refused with an error";
        usleep(500 * 1000);
        const ViewFacts after = facts(withScript);
        if (after.finished != before.finished || after.title == "Bad")
            return std::string("a load with the base URL ") + base + " loaded";
        note("BASE:%s:refused:%s", base, after.error.c_str());
    }
    // The inert ones work.
    // A data: base that WebKit spells differently (upper-case scheme, a space) is still the owner's own load.
    const char* const good[] = { "about:blank", "data:text/html,x", "", "DATA:text/html,a b" };
    for (const char* base : good) {
        const int finished = facts(withScript).finished;
        loadHtml(withScript, page("Good", "#0a0"), base);
        if (!waitView(withScript, [&](const ViewFacts& f) { return f.finished > finished && f.title == "Good" && f.error.empty(); }, 30000))
            return std::string("a load with the base URL '") + base + "' did not load: " + facts(withScript).error;
    }
    if (listener.accepted() != baseline)
        return "the listener was reached by a refused load: " + listener.requests();
    note("NETWORK:base URLs refused=%zu:allowed=%zu", sizeof bad / sizeof *bad, sizeof good / sizeof *good);
    destroy(withScript);
    destroy(withoutScript);
    return "";
}

} // namespace webdtest
