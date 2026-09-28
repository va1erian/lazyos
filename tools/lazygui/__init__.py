"""Tkinter launcher for LazyOS (build + boot + tests + sessions).

The package is split so no module exceeds the repo's 500-line file budget:

* ``catalog``  - modes, session scripts, image switches, command planning
* ``runner``   - process execution, output streaming, tree-kill helpers
* ``ui``       - the Tkinter window that ties the two together
"""

from .ui import main

__all__ = ["main"]
