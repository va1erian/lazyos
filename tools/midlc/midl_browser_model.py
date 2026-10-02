"""midl_browser model: discovery, loading and filtering, without Tk.

Part of `midl_browser.py`, split out so the data side can be tested headless
(`test_midl_browser.py`) and the GUI file stays small.
"""

from __future__ import annotations

import os
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import midlc  # noqa: E402
import midlc_transfers  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[2]
SKIP_DIRS = {".git", ".claude", "target", "node_modules", "__pycache__", ".venv"}


@dataclass
class Loaded:
    """One interface of a `.midl` file, or the file's parse error. A file
    with several interfaces (`input.midl`, `topics.midl`) yields one entry per
    interface, in source order."""

    path: Path
    interface: midlc.Interface | None = None
    error: str = ""


@dataclass
class Node:
    kind: str  # file | interface | group | method | struct | enum | topic | ring
    payload: object
    path: Path | None = None


def discover(roots: list[Path]) -> list[Path]:
    """Every `.midl` under `roots`. Only directories *below* a root are
    skipped (`SKIP_DIRS`): a root may itself live under one, such as a git
    worktree in `.claude/worktrees/`."""
    found: list[Path] = []
    for root in roots:
        root = Path(root)
        if root.is_file() and root.suffix == ".midl":
            found.append(root)
        elif root.is_dir():
            chunk: list[Path] = []
            for current, dirs, files in os.walk(root):
                dirs[:] = sorted(d for d in dirs if d not in SKIP_DIRS)
                for name in sorted(files):
                    if name.endswith(".midl"):
                        chunk.append(Path(current) / name)
            found.extend(sorted(chunk))
    seen: set[Path] = set()
    unique: list[Path] = []
    for path in found:
        resolved = path.resolve()
        if resolved in seen:
            continue
        seen.add(resolved)
        unique.append(path)
    return unique


def load(paths: list[Path]) -> list[Loaded]:
    loaded: list[Loaded] = []
    for path in paths:
        try:
            text = path.read_text(encoding="utf-8")
            interfaces = midlc.Parser(midlc.lex(text)).parse_interfaces()
            loaded.extend(Loaded(path, interface) for interface in interfaces)
        except (midlc.MidlError, OSError, UnicodeDecodeError) as error:
            loaded.append(Loaded(path, None, str(error)))
    return loaded


def signature(method: midlc.Method) -> str:
    args = ", ".join(f"{p.name}: {p.ty}" for p in method.params)
    rets = ", ".join(f"{p.name}: {p.ty}" for p in method.returns)
    return f"({args}) -> ({rets}){midlc_transfers.signature(method)}"


def kind_of(method: midlc.Method) -> str:
    """A oneway method is an event (fire-and-forget); the rest are calls."""
    return "event" if method.oneway else "method"


def ring_summary(ring: midlc.Ring) -> str:
    """`frames, produced by server, doorbell Notify` for one ring."""
    moves = f"doorbell {ring.doorbell}" if ring.doorbell else f"advance {ring.advance}"
    return f"{ring.layout}, produced by {ring.producer}, {moves}"


def matches(query: str, *texts: str) -> bool:
    if not query:
        return True
    return any(query in text.lower() for text in texts)


def filtered(interface: midlc.Interface, query: str, *context: str) -> midlc.Interface | None:
    """`interface` cut down to what matches `query` (all of it when the
    interface itself or `context` matches), or `None` when nothing does. A
    method also matches on its transfers, so `Channel`, `Ring` or an interface
    name finds the methods that carry them."""
    if not query or matches(query, interface.name, interface.docs, *context):
        return interface
    methods = [
        m for m in interface.methods
        if matches(query, m.name, m.doc, midlc_transfers.signature(m))
    ]
    structs = [s for s in interface.structs if matches(query, s.name, s.doc)]
    enums = [e for e in interface.enums if matches(query, e.name)]
    topics = [
        t for t in interface.topics
        if matches(query, t.name, t.source, t.payload, t.doc, *t.permissions)
    ]
    rings = [r for r in interface.rings if matches(query, r.name, r.doc, ring_summary(r))]
    if not (methods or structs or enums or topics or rings):
        return None
    return midlc.Interface(interface.name, interface.docs, methods, structs, enums, topics, rings)


def counts(loaded: list[Loaded]) -> dict[str, int]:
    """Totals for the status bar."""
    ok = [entry.interface for entry in loaded if entry.interface is not None]
    files = {entry.path.resolve() for entry in loaded}
    failed = {entry.path.resolve() for entry in loaded if entry.interface is None}
    return {
        "interfaces": len(ok),
        "files": len(files),
        "failed": len(failed),
        "methods": sum(len(i.methods) for i in ok),
        "transferring": sum(1 for i in ok for m in i.methods if m.transfers),
        "structs": sum(len(i.structs) for i in ok),
        "enums": sum(len(i.enums) for i in ok),
        "topics": sum(len(i.topics) for i in ok),
        "rings": sum(len(i.rings) for i in ok),
    }
