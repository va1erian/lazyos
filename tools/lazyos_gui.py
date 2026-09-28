#!/usr/bin/env python3
"""LazyOS Launcher - a small cross-platform (Tkinter) GUI for building and
booting LazyOS.

Run it from the repo root:

    python tools/lazyos_gui.py

See `tools/lazygui/catalog.py` for the modes and image build switches, and
`tools/lazygui/ui.py` for the window itself.
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from lazygui.ui import main  # noqa: E402


if __name__ == "__main__":
    raise SystemExit(main())
