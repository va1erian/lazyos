// What a FULL view tells the service (webd_abi.h, P2 slice 3): legacy.cpp's LegacySink with the view's id on every
// call. Main thread. Once the slot is dead (destroyed or lost) nothing is sent.
//
// Painting. A view made with deltas = 0 gets whole frames as a light view does (webd_ui_frame, the scroll offset
// told when it changes). With deltas = 1 the engine's paint bookkeeping goes to the service (webd_paint_*: it
// keeps what changed between two paints and answers the plan), what changed comes back as webd_ui_frame_delta,
// overscan rows as webd_ui_band, and webd_ui_scroll comes before EVERY post with the offset its pixels were
// painted at (paint.cpp tells it before each paint). A plan whose rectangles leave the view is not painted: the
// paint is a whole one instead (the pixels are written where the rectangles say).

#include <string.h>
#include <string>

#include "engine.h"
#include "lazyos_ui.h"
#include "slots.h"
#include "webd_abi.h"

namespace webkitapp {
namespace webd {

// The header's numbers are the engine's and lazyos_ui.h's: each call passes them through as they are.
static_assert(WEBD_FEATURE_PAGE_UI == LZW_FEATURE_PAGE_UI && WEBD_FEATURE_DIALOGS == LZW_FEATURE_DIALOGS
        && WEBD_FEATURE_DOWNLOADS == LZW_FEATURE_DOWNLOADS && WEBD_FEATURE_POLICY == LZW_FEATURE_POLICY
        && WEBD_FEATURE_NOTIFICATIONS == LZW_FEATURE_NOTIFICATIONS && WEBD_FEATURE_BLOCKING == LZW_FEATURE_BLOCKING,
    "WEBD_FEATURE_* are LZW_FEATURE_*");
static_assert(WEBD_VERDICT_ALLOW == LZW_VERDICT_ALLOW && WEBD_VERDICT_DENY == LZW_VERDICT_DENY
        && WEBD_VERDICT_HANDOFF == LZW_VERDICT_HANDOFF && WEBD_VERDICT_ASK == ViewSink::kVerdictAsk,
    "WEBD_VERDICT_* are the engine's verdicts");
static_assert(WEBD_PAINT_NOTHING == ViewSink::kPaintNothing && WEBD_PAINT_FULL == ViewSink::kPaintFull
        && WEBD_PAINT_DELTA == ViewSink::kPaintDelta && WEBD_PAINT_DELTA == LZW_PAINT_DELTA,
    "WEBD_PAINT_* are the engine's plans");

namespace {

// A page state as last sent: one that says nothing new is not sent again.
struct Sent {
    std::string url, title, error, committed;
    int loading { -1 }, canBack { -1 }, canForward { -1 }, progress { -1 }, secure { -1 }, finished { -1 }, serial { -1 };

    bool same(const ViewState& s) const
    {
        return loading == s.loading && canBack == s.canBack && canForward == s.canForward && progress == s.progress
            && secure == s.secure && finished == s.finished && serial == s.serial && url == s.url && title == s.title
            && error == s.error && committed == s.committedUrl;
    }
    void take(const ViewState& s)
    {
        url = s.url, title = s.title, error = s.error, committed = s.committedUrl;
        loading = s.loading, canBack = s.canBack, canForward = s.canForward, progress = s.progress;
        secure = s.secure, finished = s.finished, serial = s.serial;
    }
};

// Whether `x, y, width, height` lies inside a view of `w` by `h`.
bool inside(int x, int y, int width, int height, int w, int h)
{
    return x >= 0 && y >= 0 && width >= 0 && height >= 0 && x <= w - width && y <= h - height;
}

class FullSink final : public ViewSink {
public:
    explicit FullSink(Slot& slot)
        : m_slot(slot)
    {
    }

    void state(const ViewState& s) override
    {
        if (dead())
            return;
        if (s.crashed) {
            lose(m_slot, lostReason(s.terminated));
            return;
        }
        if (m_sent.same(s))
            return;
        m_sent.take(s);
        webd_page_state page = { };
        page.struct_size = sizeof page;
        page.url = s.url;
        page.title = s.title;
        page.loading = s.loading ? 1 : 0;
        page.can_back = s.canBack ? 1 : 0;
        page.can_forward = s.canForward ? 1 : 0;
        page.progress = s.progress;
        page.error = s.error;
        page.committed_url = s.committedUrl;
        page.secure = s.secure ? 1 : 0;
        page.finished = s.finished;
        webd_ui_page_state(m_slot.id, &page);
    }

    void responsive(bool responsive) override
    {
        const int now = responsive ? 1 : 0;
        if (dead() || now == m_responsive)
            return;
        m_responsive = now;
        webd_ui_responsive(m_slot.id, now);
    }

    // ---- painting ----
    void paintResize(int width, int height) override
    {
        if (deltas())
            webd_paint_resize(m_slot.id, width, height);
    }
    void paintFull() override
    {
        if (deltas())
            webd_paint_full(m_slot.id);
    }
    void paintRect(int x, int y, int width, int height) override
    {
        if (deltas())
            webd_paint_rect(m_slot.id, x, y, width, height);
    }
    void paintUpdate(int sx, int sy, int sw, int sh, int dx, int dy, const int* rects, int count) override
    {
        if (deltas())
            webd_paint_update(m_slot.id, sx, sy, sw, sh, dx, dy, rects, count);
    }
    int paintPlan(int* scroll, int* rects, int max, int* count) override
    {
        *count = 0;
        if (dead() || !m_slot.visible)
            return kPaintNothing;
        if (!m_slot.deltas)
            return kPaintFull;
        const int plan = webd_paint_plan(m_slot.id, scroll, rects, max, count);
        if (plan == kPaintNothing || plan == kPaintFull) {
            *count = 0;
            return plan;
        }
        if (plan == kPaintDelta && planFits(scroll, rects, max, *count))
            return kPaintDelta;
        say("PAINT:PLAN:REFUSED:a whole frame instead");
        *count = 0;
        return kPaintFull;
    }
    void frame(const uint8_t* rgba, int width, int height) override
    {
        if (!dead() && m_slot.visible)
            webd_ui_frame(m_slot.id, rgba, width, height);
    }
    int frameDelta(const uint8_t* rgba, int width, int height, const int* scroll, const int* rects, int count) override
    {
        if (dead() || !m_slot.visible || !m_slot.deltas)
            return 1;
        return webd_ui_frame_delta(m_slot.id, rgba, width, height, scroll, rects, count);
    }
    void band(int docX, int docY, int width, int height, const uint8_t* rgba) override
    {
        if (deltas() && m_slot.visible)
            webd_ui_band(m_slot.id, docX, docY, width, height, rgba);
    }
    void scrollOffset(int x, int y, bool atBottom) override
    {
        if (dead())
            return;
        // A delta view's service shows its picture at the offset of the post that follows: told every time.
        if (!m_slot.deltas && x == m_scrollX && y == m_scrollY && static_cast<int>(atBottom) == m_atBottom)
            return;
        m_scrollX = x;
        m_scrollY = y;
        m_atBottom = atBottom ? 1 : 0;
        webd_ui_scroll(m_slot.id, x, y, atBottom ? 1 : 0);
    }

    // ---- the page's UI ----
    void hover(const char* url) override
    {
        if (!dead())
            webd_ui_hover(m_slot.id, url);
    }
    void cursor(int shape) override
    {
        if (!dead())
            webd_ui_cursor(m_slot.id, shape);
    }
    void newWindow(const char* url, bool gesture) override
    {
        if (!dead())
            webd_ui_new_window(m_slot.id, url, gesture ? 1 : 0);
    }
    void dialog(uint64_t id, int kind, const char* message, const char* defaultValue) override
    {
        if (!dead())
            webd_ui_dialog(m_slot.id, id, kind, message, defaultValue);
    }

    // ---- downloads ----
    int downloadStart(uint64_t id, const char* url, const char* name, int64_t total, char* path, size_t pathSize) override
    {
        if (dead() || pathSize == 0)
            return 1;
        path[0] = 0;
        if (webd_ui_download_start(m_slot.id, id, url, name, total, path, pathSize) != 0)
            return 1;
        // The NetworkProcess writes where this says: an absolute path, whole within the buffer.
        if (!memchr(path, 0, pathSize) || path[0] != '/') {
            say("DOWNLOAD:PATH:REFUSED");
            return 1;
        }
        return 0;
    }
    void downloadProgress(uint64_t id, int64_t received, int64_t total) override
    {
        if (!dead())
            webd_ui_download_progress(m_slot.id, id, received, total);
    }
    void downloadDone(uint64_t id) override
    {
        if (!dead())
            webd_ui_download_done(m_slot.id, id);
    }
    void downloadFail(uint64_t id, const char* reason) override
    {
        if (!dead())
            webd_ui_download_fail(m_slot.id, id, reason);
    }

    // ---- navigation ----
    int navigateRequest(uint64_t request, const char* url, int kind, bool gesture, bool mainFrame, const char* committed) override
    {
        if (dead())
            return WEBD_VERDICT_DENY;
        const int verdict = webd_ui_navigate(m_slot.id, request, url, kind, gesture ? 1 : 0, mainFrame ? 1 : 0, committed);
        // Anything the header does not name is a refusal.
        return verdict >= WEBD_VERDICT_ALLOW && verdict <= WEBD_VERDICT_ASK ? verdict : WEBD_VERDICT_DENY;
    }

    // ---- notifications ----
    void notifyShow(uint64_t id, const char* origin, const char* pageUrl, const char* title, const char* body, const char* tag) override
    {
        if (!dead())
            webd_ui_notify_show(m_slot.id, id, origin, pageUrl, title, body, tag);
    }
    void notifyCancel(uint64_t id) override
    {
        if (!dead())
            webd_ui_notify_cancel(m_slot.id, id);
    }
    void notifyGone(uint64_t id) override
    {
        if (!dead())
            webd_ui_notify_gone(m_slot.id, id);
    }
    void notifyPermissionRequest(const char* origin) override
    {
        if (!dead())
            webd_ui_notify_permission_request(m_slot.id, origin);
    }
    void notifyPermissionGone(const char* origin) override
    {
        if (!dead())
            webd_ui_notify_permission_gone(m_slot.id, origin);
    }

    // ---- content blocking ----
    int blocking(const char* url) override { return dead() ? 1 : webd_ui_blocking(m_slot.id, url); }
    void blockingList(const char* id, int status) override
    {
        if (!dead())
            webd_ui_blocking_list(m_slot.id, id, status);
    }
    void blocked(int count) override
    {
        if (!dead())
            webd_ui_blocked(m_slot.id, count);
    }

private:
    bool dead() const { return m_slot.dead; }
    bool deltas() const { return m_slot.deltas && !m_slot.dead; }

    // A delta plan the engine may paint: at most `max` rectangles, each inside the view, and a scroll area inside it.
    bool planFits(const int* scroll, const int* rects, int max, int count) const
    {
        View* view = viewById(m_slot.engine);
        if (!view || count < 0 || count > max)
            return false;
        for (int i = 0; i < count; ++i) {
            if (!inside(rects[4 * i], rects[4 * i + 1], rects[4 * i + 2], rects[4 * i + 3], view->width, view->height))
                return false;
        }
        const bool moved = scroll[2] > 0 && scroll[3] > 0 && (scroll[4] || scroll[5]);
        if (!moved)
            return true;
        return inside(scroll[0], scroll[1], scroll[2], scroll[3], view->width, view->height) && scroll[4] > -view->width
            && scroll[4] < view->width && scroll[5] > -view->height && scroll[5] < view->height;
    }

    Slot& m_slot;
    Sent m_sent;
    int m_responsive { 1 };
    int m_scrollX { -1 }, m_scrollY { -1 }, m_atBottom { -1 };
};

} // namespace

std::unique_ptr<ViewSink> makeFullSink(Slot& slot) { return std::make_unique<FullSink>(slot); }

} // namespace webd
} // namespace webkitapp
