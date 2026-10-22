// webd's engine half, inside (abi.cpp, abi_full.cpp, sink_full.cpp): the slot of each service-chosen view id and
// the helpers every webd_engine_* call goes through. Main thread only, except where a function says otherwise.

#pragma once

#include <functional>
#include <memory>
#include <stdint.h>
#include <string>
#include <vector>

#include "engine.h"
#include "webd_abi.h"

namespace webkitapp {
namespace webd {

class Env final : public HostEnv {
public:
    int features() override { return 0; }
    void say(const char* line) override { webd_ui_say(line); }
    bool probe() override { return false; }
    int userMemory(uint64_t* used, uint64_t* limit) override { return webd_ui_user_memory(used, limit); }
    // The decisions every profile kept, as WebKit wants them when a page process starts (P4: a client tells webd
    // its kept decisions when it makes a view, before a page process may have started).
    int notifyPermissionAt(size_t index, char* origin, size_t size) override
    {
        return webd_ui_notify_permission_at(index, origin, size);
    }
};

extern Env env;

struct Slot;

// A call that waits for the view to be made: it runs with the slot and the engine's view.
using Call = std::function<void(Slot&, View&)>;

// What the service has asked for one id, and the engine's view once it is made.
struct Slot {
    Slot(uint64_t id, const ViewConfig& config, bool deltas)
        : id(id)
        , config(config)
        , deltas(deltas)
    {
    }

    const uint64_t id;
    const ViewConfig config;
    // A full view that sends what changed (webd_paint_*), not whole frames only.
    const bool deltas;
    // Light (abi.cpp) or full (sink_full.cpp); made with the slot.
    std::unique_ptr<ViewSink> sink;
    int engine { 0 };
    bool ready { false };
    bool dead { false };
    bool visible { true };
    std::vector<Call> pending;
};

// Once per process: webd_engine_start and webd_engine_start_host share it. Any thread (the first call).
bool claimStart();

Slot* find(uint64_t id);
// Makes the slot of a new id, with the sink its kind needs; null for an id in use.
Slot* addSlot(uint64_t id, const ViewConfig& config, bool deltas);
// The view cannot go on: the service is told once (webd_ui_view_lost), and the id is dropped.
void lose(Slot& slot, int reason);
// WEBD_LOST_* for ViewState::terminated.
int lostReason(const char* terminated);
// Makes the engine's view for a slot (the light lock settled, for a light one) and runs the calls that waited.
void finishCreate(uint64_t id);
// Runs `call` on the main thread with the view of `id`, or when it is made; an unknown or dead id ignores it.
// Any thread.
void withView(uint64_t id, Call call);
// The same for a full view only: a call that is a full view's (navigation, downloads, ...) does nothing to a
// light one. Any thread.
void withFullView(uint64_t id, Call call);

// sink_full.cpp: the sink of a full view.
std::unique_ptr<ViewSink> makeFullSink(Slot& slot);

// abi_full.cpp: a full view's configuration from the service's (`config` has struct_size bytes, read in the
// calling thread). False when it is refused (said why).
bool fullConfig(const webd_view_config& local, ViewConfig& out, bool& deltas);

} // namespace webd
} // namespace webkitapp
