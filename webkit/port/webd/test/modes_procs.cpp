// many, lost and cycle: the engine's processes. A group's views share one WebProcess, a lost process is
// told to the service and its ids are dropped, and a view that comes and goes leaves no process.

#include "support.h"

#include <algorithm>
#include <signal.h>
#include <stdlib.h>
#include <stdio.h>
#include <unistd.h>

namespace webdtest {

namespace {

const char* const kColours[3] = { "#f00", "#0a0", "#00f" };
const char kLetters[3] = { 'r', 'g', 'b' };

std::string describeProcesses()
{
    const Processes p = processes();
    char text[100];
    snprintf(text, sizeof text, "web launches=%d exits=%d, network launches=%d", p.webLaunches, p.webExits, p.networkLaunches);
    return text;
}

// Waits for the web process count to reach `exits`.
bool waitExits(int exits, int ms)
{
    return waitProcesses([&](const Processes& p) { return p.webExits >= exits; }, ms);
}

} // namespace

std::string modeMany()
{
    const char* configured = getenv("WEBD_TEST_VIEWS");
    const int kViews = configured ? atoi(configured) : 12;
    std::vector<uint64_t> ids(kViews);
    std::vector<std::string> titles(kViews);
    for (int i = 0; i < kViews; ++i) {
        Options options;
        options.group = 1;
        options.width = 200;
        options.height = 120;
        options.scripts = i % 2;
        ids[i] = create(options);
        // Scripts on: the page writes its own title; scripts off: the title stays. One process holds both.
        titles[i] = (i % 2 ? "S" : "T") + std::to_string(i);
        loadHtml(ids[i], page(("T" + std::to_string(i)).c_str(), kColours[i % 3], "<script>document.title='S" + std::to_string(i) + "'</script>"));
    }
    for (int i = 0; i < kViews; ++i) {
        const std::string title = titles[i];
        if (!waitView(ids[i], [&](const ViewFacts& f) { return shows(f, title.c_str(), kLetters[i % 3], 1); }, 120000)) {
            const ViewFacts f = facts(ids[i]);
            return "view " + std::to_string(i) + " did not paint its page (title '" + f.title + "' last " + f.last + " finished " + std::to_string(f.finished) + ")";
        }
    }
    const Processes up = processes();
    note("MANY:%d views painted:%s", kViews, describeProcesses().c_str());
    if (up.webLaunches != 1)
        return "twelve views of one group did not share one web process: " + describeProcesses();
    for (int i = 0; i < kViews; ++i) {
        const ViewFacts f = facts(ids[i]);
        for (char c : f.seen) {
            if (c != 'w' && c != kLetters[i % 3])
                return "view " + std::to_string(i) + " painted another view's colour: " + f.seen;
        }
    }

    // Destroyed in an order that is not the order made: evens up, then odds down, the last one standing.
    std::vector<int> order;
    for (int i = 0; i < kViews; i += 2)
        order.push_back(i);
    for (int i = (kViews - 1) | 1; i >= 1; i -= 2)
        if (i < kViews)
            order.push_back(i);
    for (int n = 0; n < kViews - 1; ++n) {
        destroy(ids[order[n]]);
        note("MANY:destroyed %d of %d", n + 1, kViews);
    }
    if (processes().webExits != 0)
        return "destroying views of a group ended the process the survivor uses: " + describeProcesses();
    const uint64_t last = ids[order[kViews - 1]];
    loadHtml(last, page("After", "#0a0"));
    if (!waitView(last, [](const ViewFacts& f) { return shows(f, "After", 'g', 2); }, 60000))
        return "the last view did not paint after the others went: " + describeProcesses();
    destroy(last);
    if (!waitExits(1, 30000))
        return "the group's web process did not end with its last view: " + describeProcesses();
    for (int i = 0; i < kViews; ++i) {
        if (facts(ids[i]).violations)
            return "view " + std::to_string(i) + " was told something after view_destroy";
    }
    note("MANY:destroyed in a different order:%s", describeProcesses().c_str());
    return processes().webExits == 1 ? "" : "web processes ended: " + describeProcesses();
}

std::string modeLost()
{
    Options options;
    options.width = 200;
    options.height = 120;
    const uint64_t v = create(options);
    loadHtml(v, page("One", "#f00"));
    if (!waitView(v, [](const ViewFacts& f) { return shows(f, "One", 'r', 1); }, 60000))
        return "the first page did not show";
    int web = 0, network = 0;
    webd_engine_children(&web, &network);
    if (web <= 0)
        return "no web process id";
    note("LOST:killing web process %d", web);
    kill(web, SIGKILL);
    if (!waitView(v, [](const ViewFacts& f) { return f.lost; }, 60000))
        return "no view_lost after the web process was killed";
    if (facts(v).lostReason != WEBD_LOST_CRASHED)
        return "view_lost reason " + std::to_string(facts(v).lostReason) + ", not CRASHED";
    // The id is dead: calls about it do nothing and nothing is told.
    loadHtml(v, page("Ghost", "#0a0"));
    webd_engine_resize(v, 50, 50);
    webd_engine_refresh(v);
    usleep(1500 * 1000);
    if (facts(v).violations)
        return "the engine told something about a lost view";
    webd_engine_view_destroy(v);

    // The host goes on: a new view works.
    const uint64_t again = create(options);
    loadHtml(again, page("Two", "#0a0"));
    if (!waitView(again, [](const ViewFacts& f) { return shows(f, "Two", 'g', 1); }, 60000))
        return "a view made after the loss did not paint";

    // A group's one process: every view of it is lost, and the group is rebuilt with new views.
    Options group = options;
    group.group = 7;
    const uint64_t a = create(group), b = create(group);
    loadHtml(a, page("A", "#f00"));
    loadHtml(b, page("B", "#00f"));
    if (!waitView(a, [](const ViewFacts& f) { return shows(f, "A", 'r', 1); }, 60000) || !waitView(b, [](const ViewFacts& f) { return shows(f, "B", 'b', 1); }, 60000))
        return "the group's views did not paint";
    const Processes before = processes();
    webd_engine_children(&web, &network);
    note("LOST:killing the group's web process %d", web);
    kill(web, SIGKILL);
    if (!waitView(a, [](const ViewFacts& f) { return f.lost; }, 60000) || !waitView(b, [](const ViewFacts& f) { return f.lost; }, 60000))
        return "not every view of the group was lost";
    if (facts(a).lostReason != WEBD_LOST_CRASHED || facts(b).lostReason != WEBD_LOST_CRASHED)
        return "a group view's reason is not CRASHED";
    const uint64_t c = create(group);
    loadHtml(c, page("C", "#0a0"));
    if (!waitView(c, [](const ViewFacts& f) { return shows(f, "C", 'g', 1); }, 60000))
        return "the rebuilt group's view did not paint";
    note("LOST:group rebuilt:web launches %d -> %d", before.webLaunches, processes().webLaunches);
    for (uint64_t id : { a, b })
        if (facts(id).violations)
            return "the engine told something about a lost group view";
    destroy(again);
    destroy(c);
    return "";
}

std::string modeCycle()
{
    // A client's views all share one group (the service gives one group per client), so a destroyed
    // view's page proxy is pooled and the next create of the same class adopts it instead of making a
    // new proxy: a create/destroy loop must not grow webd (docs/webd-plan.md P5, "What still grows
    // without bound"). The group is fixed here, as a real client's is.
    constexpr int kGroup = 7;

    // Part 1: paint a view, destroy it, and do it again, a few times. Each later view adopts the pooled
    // proxy (poolReused grows), still paints, and the proxy table stays at one pooled, none retired.
    constexpr int kPaintCycles = 16;
    for (int i = 0; i < kPaintCycles; ++i) {
        Options options;
        options.group = kGroup;
        options.width = 160;
        options.height = 100;
        const uint64_t v = create(options);
        const std::string title = "Cycle" + std::to_string(i);
        loadHtml(v, page(title.c_str(), kColours[i % 3]));
        if (!waitView(v, [&](const ViewFacts& f) { return shows(f, title.c_str(), kLetters[i % 3], 1); }, 90000))
            return "view " + std::to_string(i) + " did not paint: " + describeProcesses();
        destroy(v);
        if (facts(v).violations || facts(v).lost)
            return "view " + std::to_string(i) + " was told something after view_destroy";
        const PoolCounts pool = poolCounts();
        if (pool.pooled != 1 || pool.retired != 0)
            return "after destroy the pool was not one proxy and none retired: pooled=" + std::to_string(pool.pooled) + " retired=" + std::to_string(pool.retired);
    }
    PoolCounts afterPaint = poolCounts();
    note("CYCLE:%d painted views, pool reused=%ld pooled=%ld retired=%ld:%s", kPaintCycles, afterPaint.reused, afterPaint.pooled, afterPaint.retired, describeProcesses().c_str());
    // The first view made the proxy; every one after it reused the pooled one.
    if (afterPaint.reused < kPaintCycles - 1)
        return "painted cycles did not reuse the pooled proxy: reused=" + std::to_string(afterPaint.reused) + " of " + std::to_string(kPaintCycles);

    // Part 2: many create/destroy iterations without a load (so no web process starts, and the loop is
    // cheap): the proxy table must be flat after the warm-up. This is the growth the pool fixes: before
    // it, every destroyed view left a WebPageProxy forever, so webd grew without bound.
    constexpr int kFastCycles = 3000;
    const long reusedBefore = poolCounts().reused;
    for (int i = 0; i < kFastCycles; ++i) {
        Options options;
        options.group = kGroup;
        options.width = 160;
        options.height = 100;
        const uint64_t v = create(options);
        destroy(v);
    }
    const PoolCounts pool = poolCounts();
    note("CYCLE:%d fast create/destroy, pool reused=%ld pooled=%ld live=%ld retired=%ld", kFastCycles, pool.reused, pool.pooled, pool.live, pool.retired);
    // Flat means the proxy pool never grows past one and none is retired (an overflow), whatever the
    // last snapshot's live count is (it is 0 or 1 depending on whether the last marker was a create or a
    // destroy). Without the pool the parked-proxy count would be `kFastCycles`.
    if (pool.pooled > 1 || pool.retired != 0)
        return "the proxy pool grew over a create/destroy loop: pooled=" + std::to_string(pool.pooled) + " retired=" + std::to_string(pool.retired);
    if (pool.reused < reusedBefore + kFastCycles)
        return "a fast cycle did not reuse the pooled proxy every time: reused grew by " + std::to_string(pool.reused - reusedBefore) + " over " + std::to_string(kFastCycles);
    note("CYCLE:the proxy pool stayed flat over %d create/destroy iterations", kPaintCycles + kFastCycles);
    return "";
}

} // namespace webdtest
