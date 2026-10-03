"""A vertically scrolling pane for the launcher's tabs."""

from __future__ import annotations

import tkinter as tk
from tkinter import ttk


def scrollable(parent: ttk.Frame, width: int = 470) -> ttk.Frame:
    """Wrap ``parent`` in a vertically scrolling canvas; return the inner frame.

    The wheel is bound application-wide (a child widget under the pointer
    would otherwise swallow it), so several panes coexist: every pane's
    handler runs, and only the one on screen (its tab selected) scrolls.
    """
    canvas = tk.Canvas(parent, borderwidth=0, highlightthickness=0, width=width)
    vsb = ttk.Scrollbar(parent, orient="vertical", command=canvas.yview)
    inner = ttk.Frame(canvas)
    win = canvas.create_window((0, 0), window=inner, anchor="nw")
    canvas.configure(yscrollcommand=vsb.set)
    canvas.pack(side="left", fill="both", expand=True)
    vsb.pack(side="right", fill="y")
    inner.bind("<Configure>", lambda e: canvas.configure(scrollregion=canvas.bbox("all")))
    canvas.bind("<Configure>", lambda e: canvas.itemconfigure(win, width=e.width))

    def on_wheel(event: tk.Event) -> None:
        """Scroll this pane while it is shown; X11 reports the wheel as Button-4/5."""
        if not canvas.winfo_ismapped():
            return
        if event.num == 4:
            canvas.yview_scroll(-1, "units")
        elif event.num == 5:
            canvas.yview_scroll(1, "units")
        elif event.delta:
            canvas.yview_scroll(-1 if event.delta > 0 else 1, "units")

    for sequence in ("<MouseWheel>", "<Button-4>", "<Button-5>"):
        canvas.bind_all(sequence, on_wheel, add="+")
    return inner
