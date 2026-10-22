// webkit-smoke's spawn probe (issue #889): no program the UI process starts while an engine process is
// being launched may inherit the engine's end of its IPC connection. See spawn.cpp.

#pragma once

namespace smoke {

// The probe child: when argv[1] is "--fd-probe" this scans the descriptors, prints
// WEBKIT:W4:SPAWN:LEAK:fd=<n> for each socket it holds and exits (status 1 on a leak, else 0).
// Call it first thing in main; it returns when the program is not the probe.
void runChildProbeIfAsked(int argc, char** argv);

// What this program's `lzw_launcher_event` does at a launch (spawn.cpp): starts the probe child and
// waits for it, failing the run on a leak.
void runProbeAtLaunch();

// Runs the probe against a socket pair that is inheritable on purpose and requires it to see it
// (WEBKIT:W4:SPAWN:CONTROL:PASS); call before the engine is launched.
void startSpawnProbe();

// Checks that the probe ran at each launch and found nothing, prints
// WEBKIT:W4:SPAWN:PASS:launches=<n>, or fails the run (WEBKIT:W4:FAIL:spawn:...).
void finishSpawnProbe();

} // namespace smoke
