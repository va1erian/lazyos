// The C ABI of lazyos_ui.h over the engine: one window, one page. The window (a Rust static library)
// implements the lzw_ui_* functions, this file turns them into the ViewSink and HostEnv the engine
// speaks and answers its lzw_engine_* calls on view 1, the one page of the process. A second owner of
// the engine (webd, the two-view test) implements those interfaces itself and does not link this file.

#include <WebKit/WKRunLoop.h>

#include <stdint.h>
#include <string>

#include "engine.h"
#include "legacy.h"
#include "lazyos_ui.h"

namespace webkitapp {

namespace {

// The one view of this process: the first view made gets id 1.
constexpr int kView = 1;

class LegacyEnv final : public HostEnv {
public:
    int features() override
    {
        if (!m_known) {
            m_features = lzw_ui_features();
            m_known = true;
        }
        return m_features;
    }
    void say(const char* line) override { lzw_ui_say(line); }
    bool probe() override { return lzw_ui_probe_enabled() != 0; }
    int userMemory(uint64_t* used, uint64_t* limit) override { return lzw_ui_user_memory(used, limit); }
    int notifyPermissionAt(size_t index, char* origin, size_t size) override { return lzw_ui_notify_permission_at(index, origin, size); }

private:
    int m_features { 0 };
    bool m_known { false };
};

class LegacySink final : public ViewSink {
public:
    void state(const ViewState& s) override
    {
        lzw_ui_state(s.url, s.title, s.loading ? 1 : 0, s.canBack ? 1 : 0, s.canForward ? 1 : 0, s.progress, s.error,
            s.crashed ? 1 : 0, s.committedUrl, s.secure ? 1 : 0, s.finished);
    }
    void responsive(bool responsive) override { lzw_ui_page_responsive(responsive ? 1 : 0); }

    void paintResize(int width, int height) override { lzw_paint_resize(width, height); }
    void paintFull() override { lzw_paint_full(); }
    void paintRect(int x, int y, int width, int height) override { lzw_paint_rect(x, y, width, height); }
    void paintUpdate(int sx, int sy, int sw, int sh, int dx, int dy, const int* rects, int count) override
    {
        lzw_paint_update(sx, sy, sw, sh, dx, dy, rects, count);
    }
    int paintPlan(int* scroll, int* rects, int max, int* count) override { return lzw_paint_plan(scroll, rects, max, count); }
    void frame(const uint8_t* rgba, int width, int height) override { lzw_ui_frame(rgba, width, height); }
    int frameDelta(const uint8_t* rgba, int width, int height, const int* scroll, const int* rects, int count) override
    {
        return lzw_ui_frame_delta(rgba, width, height, scroll, rects, count);
    }
    void band(int docX, int docY, int width, int height, const uint8_t* rgba) override { lzw_ui_band(docX, docY, width, height, rgba); }
    void scrollOffset(int x, int y, bool atBottom) override { lzw_ui_scroll_offset(x, y, atBottom ? 1 : 0); }

    void hover(const char* url) override { lzw_ui_hover(url); }
    void cursor(int shape) override { lzw_ui_cursor(shape); }
    void newWindow(const char* url, bool gesture) override { lzw_ui_new_window(url, gesture ? 1 : 0); }
    void dialog(uint64_t id, int kind, const char* message, const char* defaultValue) override
    {
        lzw_ui_dialog(id, kind, message, defaultValue);
    }
    int downloadStart(uint64_t id, const char* url, const char* name, int64_t total, char* path, size_t pathSize) override
    {
        return lzw_ui_download_start(id, url, name, total, path, pathSize);
    }
    void downloadProgress(uint64_t id, int64_t received, int64_t total) override { lzw_ui_download_progress(id, received, total); }
    void downloadDone(uint64_t id) override { lzw_ui_download_done(id); }
    void downloadFail(uint64_t id, const char* reason) override { lzw_ui_download_fail(id, reason); }
    int navigate(const char* url, int kind, bool gesture, const char* committed) override
    {
        return lzw_ui_navigate(url, kind, gesture ? 1 : 0, committed);
    }

    void notifyShow(uint64_t id, const char* origin, const char* pageUrl, const char* title, const char* body, const char* tag) override
    {
        lzw_ui_notify_show(id, origin, pageUrl, title, body, tag);
    }
    void notifyCancel(uint64_t id) override { lzw_ui_notify_cancel(id); }
    void notifyGone(uint64_t id) override { lzw_ui_notify_gone(id); }
    void notifyPermissionRequest(const char* origin) override { lzw_ui_notify_permission_request(origin, 1); }
    void notifyPermissionGone(const char* origin) override { lzw_ui_notify_permission_gone(origin); }

    int blocking(const char* url) override { return lzw_ui_blocking(url); }
    void blockingList(const char* id, int status) override { lzw_ui_blocking_list(id, status); }
    void blocked(int count) override { lzw_ui_blocked(count); }
};

LegacyEnv env;
LegacySink sink;

// The window's profile directory, as it asks for it (LZW_FEATURE_PROFILE), or empty for none.
std::string profileDirectory()
{
    if (!(env.features() & LZW_FEATURE_PROFILE))
        return std::string();
    const char* chosen = lzw_ui_profile_dir();
    if (!chosen || *chosen != '/')
        return std::string();
    std::string path(chosen);
    while (path.size() > 1 && path.back() == '/')
        path.pop_back();
    return path;
}

} // namespace

void useLegacyEnvironment() { H.env = &env; }

} // namespace webkitapp

using namespace webkitapp;

extern "C" {

int lzw_engine_start(void)
{
    const std::string profile = profileDirectory();
    if (!startHost(&env, profile))
        return 1;
    ViewConfig config;
    config.features = env.features();
    config.profile = profile;
    return createView(config, &sink) ? 0 : 1;
}

int lzw_engine_running(void) { return H.running ? 1 : 0; }

void lzw_engine_quit(void) { WKRunLoopCallOnMainThread([](void*) { WKRunLoopStopMain(); }, nullptr); }

int lzw_engine_shutdown(void) { return shutdownChildren(); }

// The ids of the page's WebProcess and of the NetworkProcess as last seen (0: none yet); any thread.
void lzw_engine_children(int* web, int* network)
{
    if (web)
        *web = H.webPid;
    if (network)
        *network = H.networkPid;
}

void lzw_engine_run_on_main(void (*task)(void*), void* context)
{
    if (task)
        onMain([task, context] { task(context); });
}

void lzw_engine_recover(int reload) { onView(kView, [reload](View& v) { v.recover(reload != 0); }); }

void lzw_engine_load(const char* url)
{
    std::string address = url ? url : "";
    onView(kView, [address](View& v) { v.load(address); });
}

void lzw_engine_reload(void) { onView(kView, [](View& v) { v.reload(); }); }
void lzw_engine_stop(void) { onView(kView, [](View& v) { v.stop(); }); }
void lzw_engine_back(void) { onView(kView, [](View& v) { v.back(); }); }
void lzw_engine_forward(void) { onView(kView, [](View& v) { v.forward(); }); }

void lzw_engine_resize(int width, int height)
{
    onView(kView, [width, height](View& v) { v.resize(width, height); });
}

void lzw_engine_mouse_now(int kind, int button, int x, int y, uint32_t mods)
{
    if (View* v = viewById(kView))
        v->mouse(kind, button, x, y, mods);
}

void lzw_engine_mouse(int kind, int button, int x, int y, uint32_t mods)
{
    // Moves reach here only as the one the window took out of its slot ahead of a press, a release
    // or a wheel (xui-webkit's pointer.rs): the others come through lzw_engine_mouse_now.
    onView(kView, [=](View& v) { v.mouse(kind, button, x, y, mods); });
}

void lzw_engine_wheel(int x, int y, int ticksX, int ticksY, uint32_t mods)
{
    onView(kView, [=](View& v) { v.wheel(x, y, ticksX, ticksY, mods); });
}

void lzw_engine_key(int press, uint32_t keysym, const char* text, uint32_t mods)
{
    std::string typed = text ? text : "";
    onView(kView, [=](View& v) { v.key(press != 0, keysym, typed, mods); });
}

void lzw_engine_focus(int focused) { onView(kView, [focused](View& v) { v.focus(focused != 0); }); }

void lzw_engine_download(const char* url)
{
    std::string address = url ? url : "";
    onView(kView, [address](View& v) { v.download(address); });
}

void lzw_engine_download_cancel(uint64_t id)
{
    onView(kView, [id](View& v) { v.downloadCancel(id); });
}

void lzw_engine_dialog_reply(uint64_t id, int accepted, const char* value)
{
    std::string text = value ? value : "";
    onView(kView, [id, accepted, text](View& v) { v.replyDialog(id, accepted != 0, text); });
}

void lzw_engine_notify_shown(uint64_t id)
{
    onMain([id] { notifyShown(id); });
}

void lzw_engine_notify_clicked(uint64_t id)
{
    onMain([id] { notifyClicked(id); });
}

void lzw_engine_notify_closed(uint64_t id)
{
    onMain([id] { notifyClosed(id); });
}

void lzw_engine_notify_permission(const char* origin, int answer)
{
    const std::string text = origin ? origin : "";
    onMain([text, answer] { notifyPermission(text, answer); });
}

void lzw_engine_notify_forget(const char* origin)
{
    const std::string text = origin ? origin : "";
    onMain([text] { notifyForget(text); });
}

void lzw_engine_blocking_load(const char* id)
{
    std::string name = id ? id : "";
    onView(kView, [name](View& v) { blockingLoad(v, name); });
}

void lzw_engine_blocking_compile(const char* id, const char* json)
{
    std::string name = id ? id : "";
    std::string source = json ? json : "";
    onView(kView, [name, source = std::move(source)](View& v) { blockingCompile(v, name, source); });
}

void lzw_engine_blocking_forget(const char* id)
{
    std::string name = id ? id : "";
    onView(kView, [name](View& v) { blockingForget(v, name); });
}

} // extern "C"
