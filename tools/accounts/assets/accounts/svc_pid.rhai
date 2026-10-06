// Print the task id of the logd service (from init's supervision table), or 0.
// /proc has no per-task entries, so signal_service in attack.sh asks init.
let pid = 0;
try {
    for s in sys::init::services() {
        if s.name == "logd" { pid = s.pid; }
    }
} catch (e) {}
print(pid);
