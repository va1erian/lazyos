// The C ABI of lazyos_ui.h as the engine's owner (legacy.cpp). Only the programs that link a window
// through that ABI have it.

#pragma once

namespace webkitapp {

// Makes the window's lzw_ui_* functions the host's environment, so a marker printed before the engine
// starts (main's "UP") has somewhere to go.
void useLegacyEnvironment();

} // namespace webkitapp
