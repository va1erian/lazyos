// webd's engine half (docs/webd-plan.md section 3.2, 3.6; the contract is webd_abi.h): the C ABI the service
// half calls (webd_engine_*) over the engine of webkit/port/app, one Host and any number of Views, and the
// engine's answers turned into the service's callbacks (webd_ui_*). This file holds the slots, the light views'
// sink and the calls every view takes; a full view's sink is sink_full.cpp and its own calls abi_full.cpp.
//
// The service chooses every view id; the engine's View ids are its own, so each id has a Slot (slots.h) that
// holds the engine's view, what the service has been told (so a callback that says nothing new is not sent) and
// the calls that arrived before the view was made. Every webd_engine_* call copies its arguments and queues a
// task for WebKit's main thread, where the slots live: nothing here waits for WebKit and no call about an id
// the engine does not know (never made, destroyed, lost) does anything.
//
// A light view cannot be made before the network lock is compiled (light.cpp), which takes a moment after
// startup: view_create is answered when it is, and the view's calls wait for it in order. A full view needs no
// lock and is made at once, but only by a host whose children's environment was fixed at start
// (webd_engine_start_host with a children directory): without it the first profile would decide every other's.

#include <WebKit/WKPage.h>
#include <WebKit/WKRunLoop.h>
#include <WebKit/WKString.h>
#include <WebKit/WKType.h>

#include <algorithm>
#include <atomic>
#include <map>
#include <memory>
#include <stddef.h>
#include <string.h>
#include <string>
#include <utility>
#include <vector>

#include "engine.h"
#include "slots.h"
#include "webd_abi.h"

namespace webkitapp {
namespace webd {

Env env;

namespace {

constexpr int kMaxSide = 8192;
constexpr size_t kMaxStyle = 64 * 1024;
constexpr int kMaxFont = 512;
// The X11 keysyms of C and c: the Copy shortcut.
constexpr uint32_t kKeysymC = 0x43;
constexpr uint32_t kKeysymLowerC = 0x63;
constexpr uint32_t kModControl = 1u << 1;

// What one light view tells the service. Main thread. Once the slot is dead (destroyed or lost) nothing is sent.
class Sink final : public ViewSink {
public:
    explicit Sink(Slot& slot)
        : m_slot(slot)
    {
    }

    void state(const ViewState& s) override;
    void linkClicked(const char* url) override;
    // Whole frames only; a view that is not visible gets none (its page still runs).
    int paintPlan(int*, int*, int, int* count) override;
    void frame(const uint8_t* rgba, int width, int height) override;
    void scrollOffset(int x, int y, bool atBottom) override;

private:
    Slot& m_slot;
    bool m_stateSent { false };
    std::string m_title, m_error;
    int m_loading { 0 }, m_finished { 0 }, m_serial { 0 };
    int m_scrollX { -1 }, m_scrollY { -1 }, m_atBottom { -1 };
};

// Main thread only.
std::map<uint64_t, std::unique_ptr<Slot>> slots;

// Ends the engine's view and forgets the slot. Never from inside one of the view's own sink calls (the
// code that called the sink carries on): the callers queue it.
void forget(uint64_t id)
{
    Slot* slot = find(id);
    if (!slot)
        return;
    slot->dead = true;
    if (slot->ready)
        destroyView(slot->engine);
    slots.erase(id);
}

void Sink::state(const ViewState& s)
{
    if (m_slot.dead)
        return;
    if (s.crashed) {
        lose(m_slot, lostReason(s.terminated));
        return;
    }
    const int loading = s.loading ? 1 : 0;
    if (m_stateSent && loading == m_loading && s.finished == m_finished && s.serial == m_serial && m_title == s.title && m_error == s.error)
        return;
    m_stateSent = true;
    m_serial = s.serial;
    m_loading = loading;
    m_finished = s.finished;
    m_title = s.title;
    m_error = s.error;
    webd_ui_state(m_slot.id, s.title, loading, s.finished, s.error);
}

void Sink::linkClicked(const char* url)
{
    if (!m_slot.dead)
        webd_ui_link_clicked(m_slot.id, url);
}

int Sink::paintPlan(int*, int*, int, int* count)
{
    *count = 0;
    return m_slot.visible && !m_slot.dead ? kPaintFull : kPaintNothing;
}

void Sink::frame(const uint8_t* rgba, int width, int height)
{
    if (!m_slot.dead && m_slot.visible)
        webd_ui_frame(m_slot.id, rgba, width, height);
}

void Sink::scrollOffset(int x, int y, bool atBottom)
{
    if (m_slot.dead || (x == m_scrollX && y == m_scrollY && static_cast<int>(atBottom) == m_atBottom))
        return;
    m_scrollX = x;
    m_scrollY = y;
    m_atBottom = atBottom ? 1 : 0;
    webd_ui_scroll(m_slot.id, x, y, atBottom ? 1 : 0);
}

int clampSide(int side) { return std::max(1, std::min(side, kMaxSide)); }

// The selection, read back for a Copy: script is the only way to ask the page (WebKit's C API has no selected
// text), so a view with scripts off copies nothing.
void selectionRead(WKTypeRef result, WKErrorRef, void* context)
{
    std::unique_ptr<uint64_t> id(static_cast<uint64_t*>(context));
    Slot* slot = find(*id);
    if (!slot || slot->dead || !result || WKGetTypeID(result) != WKStringGetTypeID())
        return;
    const std::string text = utf8(static_cast<WKStringRef>(result));
    if (!text.empty())
        webd_ui_clipboard_write(slot->id, text.c_str());
}

void copySelection(Slot& slot, View& view)
{
    if (!slot.config.scripts)
        return;
    WKStringRef script = WKStringCreateWithUTF8CString("String(getSelection())");
    WKPageEvaluateJavaScriptInMainFrame(view.page, script, new uint64_t(slot.id), selectionRead);
    WKRelease(script);
}

// What every view's configuration says, light or full: read now, in the caller's thread.
ViewConfig commonConfig(const webd_view_config& local)
{
    ViewConfig vc;
    vc.scripts = local.scripts == 1;
    vc.group = local.group;
    vc.width = clampSide(local.width);
    vc.height = clampSide(local.height);
    vc.colorScheme = local.color_scheme;
    vc.defaultFontSize = std::max(0, std::min(local.default_font_size, kMaxFont));
    if (local.user_style) {
        const size_t length = strnlen(local.user_style, kMaxStyle + 1);
        if (length <= kMaxStyle)
            vc.userStyle.assign(local.user_style, length);
        else
            say("CONFIG:IGNORED:user_style");
    }
    return vc;
}

} // namespace

bool claimStart()
{
    // Once: a second host would be a second WKContext and its processes.
    static std::atomic<bool> started { false };
    return !started.exchange(true);
}

Slot* find(uint64_t id)
{
    auto it = slots.find(id);
    return it == slots.end() ? nullptr : it->second.get();
}

Slot* addSlot(uint64_t id, const ViewConfig& config, bool deltas)
{
    if (find(id))
        return nullptr;
    auto slot = std::make_unique<Slot>(id, config, deltas);
    if (config.light)
        slot->sink = std::make_unique<Sink>(*slot);
    else
        slot->sink = makeFullSink(*slot);
    Slot* made = slot.get();
    slots[id] = std::move(slot);
    return made;
}

void lose(Slot& slot, int reason)
{
    if (slot.dead)
        return;
    slot.dead = true;
    webd_ui_view_lost(slot.id, reason);
    const uint64_t id = slot.id;
    onMain([id] { forget(id); });
}

int lostReason(const char* terminated)
{
    if (!strcmp(terminated, "memory"))
        return WEBD_LOST_MEMORY;
    if (!strcmp(terminated, "crash"))
        return WEBD_LOST_CRASHED;
    return WEBD_LOST_OTHER;
}

void finishCreate(uint64_t id)
{
    Slot* slot = find(id);
    if (!slot || slot->dead)
        return;
    if (slot->config.light && lightLock() != LightLock::Ready) {
        lose(*slot, WEBD_LOST_REFUSED);
        return;
    }
    View* view = createView(slot->config, slot->sink.get());
    if (!view) {
        lose(*slot, WEBD_LOST_OTHER);
        return;
    }
    slot->engine = view->id;
    slot->ready = true;
    // Success is the first state the service hears for the id.
    postState(*view);
    auto waiting = std::move(slot->pending);
    for (auto& call : waiting) {
        // A call may have ended the view (it cannot today; the check costs nothing).
        Slot* again = find(id);
        View* live = again && !again->dead ? viewById(again->engine) : nullptr;
        if (!live)
            return;
        call(*again, *live);
    }
}

void withView(uint64_t id, Call call)
{
    onMain([id, call = std::move(call)]() mutable {
        Slot* slot = find(id);
        if (!slot || slot->dead)
            return;
        if (!slot->ready) {
            slot->pending.push_back(std::move(call));
            return;
        }
        if (View* view = viewById(slot->engine))
            call(*slot, *view);
    });
}

void withFullView(uint64_t id, Call call)
{
    withView(id, [call = std::move(call)](Slot& slot, View& view) {
        if (!slot.config.light)
            call(slot, view);
    });
}

} // namespace webd
} // namespace webkitapp

using namespace webkitapp;
using namespace webkitapp::webd;

extern "C" {

int webd_engine_start(void)
{
    if (!claimStart())
        return 1;
    // No profile directory: a light view keeps nothing.
    if (!startHost(&env))
        return 1;
    prepareLightLock();
    return 0;
}

void webd_engine_view_create(uint64_t view, const webd_view_config* config)
{
    // Read now: the caller's struct and strings are valid only until this returns.
    webd_view_config local = { };
    bool refused = !config || view == 0;
    bool light = true;
    bool deltas = false;
    ViewConfig vc;
    if (!refused) {
        memcpy(&local, config, std::min<size_t>(config->struct_size, sizeof local));
        // Too short to say what the view is: refused.
        refused = config->struct_size < offsetof(webd_view_config, light) + sizeof local.light || (local.light != 0 && local.light != 1);
        // Fields past the caller's struct_size were not copied: zero, as the header promises.
        if (config->struct_size < offsetof(webd_view_config, user_style) + sizeof local.user_style)
            local.user_style = nullptr;
    }
    if (!refused) {
        light = local.light == 1;
        vc = commonConfig(local);
        vc.light = light;
        if (!light)
            refused = !fullConfig(local, vc, deltas);
    }
    onMain([view, vc = std::move(vc), refused, light, deltas]() mutable {
        if (view == 0)
            return;
        if (find(view)) {
            say("VIEW:DUPLICATE");
            return;
        }
        // A full view's profiles need the children's environment decided at start (slots.h, fullConfig).
        if (!refused && !light && !H.environmentFixed) {
            say("FULL:REFUSED:the host has no children's directory (webd_engine_start_host)");
            refused = true;
        }
        if (refused) {
            webd_ui_view_lost(view, WEBD_LOST_REFUSED);
            return;
        }
        // A profile the engine cannot make is a refusal, not a view that silently keeps nothing.
        if (!light && !vc.profile.empty() && !makePrivateDirectories(vc.profile)) {
            say("FULL:REFUSED:cannot make the profile directory");
            webd_ui_view_lost(view, WEBD_LOST_REFUSED);
            return;
        }
        addSlot(view, vc, deltas);
        if (light)
            whenLightLockSettled([view] { finishCreate(view); });
        else
            finishCreate(view);
    });
}

void webd_engine_view_destroy(uint64_t view)
{
    onMain([view] { forget(view); });
}

void webd_engine_load_html(uint64_t view, const uint8_t* html, size_t len, const char* base_url)
{
    std::string document(reinterpret_cast<const char*>(html), html ? len : 0);
    std::string base = base_url ? base_url : "";
    // A light view's one load (an inert base only); a full view's page of the client's own, under a base the
    // service checked (about:, data:, http:, https:; never file:, which the engine refuses too).
    withView(view, [document = std::move(document), base = std::move(base)](Slot& s, View& v) {
        if (s.config.light)
            v.loadHtml(document, base);
        else
            loadFullHtml(v, document, base);
    });
}

void webd_engine_resize(uint64_t view, int width, int height)
{
    withView(view, [=](Slot&, View& v) { v.resize(clampSide(width), clampSide(height)); });
}

void webd_engine_set_visible(uint64_t view, int visible)
{
    withView(view, [=](Slot& s, View& v) {
        const bool was = s.visible;
        s.visible = visible != 0;
        // What changed while it was hidden is painted now.
        if (s.visible && !was)
            schedulePaint(v);
    });
}

void webd_engine_refresh(uint64_t view)
{
    withView(view, [](Slot&, View& v) { schedulePaint(v); });
}

void webd_engine_mouse(uint64_t view, int kind, int button, int x, int y, uint32_t mods)
{
    // webd_abi.h: 0 move, 1 press, 2 release. The engine's View::mouse takes 0 down, 1 up, 2 move.
    const int engineKind = kind == 1 ? 0 : kind == 2 ? 1 : 2;
    withView(view, [=](Slot&, View& v) { v.mouse(engineKind, button, x, y, mods); });
}

void webd_engine_wheel(uint64_t view, int x, int y, int ticks_x, int ticks_y, uint32_t mods)
{
    withView(view, [=](Slot&, View& v) { v.wheel(x, y, ticks_x, ticks_y, mods); });
}

void webd_engine_key(uint64_t view, int press, uint32_t keysym, const char* text, uint32_t mods)
{
    std::string typed = text ? text : "";
    withView(view, [=, typed = std::move(typed)](Slot& s, View& v) {
        v.key(press != 0, keysym, typed, mods);
        if (press && (mods & kModControl) && (keysym == kKeysymC || keysym == kKeysymLowerC))
            copySelection(s, v);
    });
}

void webd_engine_focus(uint64_t view, int focused)
{
    withView(view, [=](Slot&, View& v) { v.focus(focused != 0); });
}

void webd_engine_run_on_main(void (*task)(void* context), void* context)
{
    if (task)
        onMain([task, context] { task(context); });
}

void webd_engine_quit(void)
{
    WKRunLoopCallOnMainThread([](void*) { WKRunLoopStopMain(); }, nullptr);
}

void webd_engine_children(int* web, int* network)
{
    if (web)
        *web = H.webPid;
    if (network)
        *network = H.networkPid;
}

} // extern "C"
