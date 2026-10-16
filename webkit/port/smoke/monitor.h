// webkit-smoke's descriptor mode (issue #867): WEBKIT_SMOKE_FDS=<n> holds n descriptors before
// WebKit starts, so the UI process's IPC sockets are above 1024, and so are the engine processes'
// ends of their connection to it (inherited at the same numbers; the WebProcess-NetworkProcess
// connection is made in the engines and stays low), and first tests the IPC socket
// monitor's own loop (SocketMonitorLazyOS.h) on such a socket. See monitor.cpp.

#pragma once

namespace smoke {

// Holds the descriptors WEBKIT_SMOKE_FDS asks for and runs the monitor test; prints
// WEBKIT:W4:FDS:... and WEBKIT:W4:MONITOR:PASS:..., or fails the run. Does nothing without it.
void runDescriptorMode();

} // namespace smoke
