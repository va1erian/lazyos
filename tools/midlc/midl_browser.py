#!/usr/bin/env python3
"""midl_browser - a small Tk GUI to consult every `.midl` definition.

Discovers all `*.midl` files under the repository (or the paths you pass),
parses them with the same parser the compiler uses (`midlc.py`), and shows a
navigable tree of interfaces, methods, structs and enums with the details
(method ids, signatures, interface hash, doc comments) in the right pane.

Usage:
    python tools/midlc/midl_browser.py                 # scan the repo
    python tools/midlc/midl_browser.py idl             # scan a directory
    python tools/midlc/midl_browser.py idl/echo.midl   # scan one file

Stdlib only (tkinter); no build step, no third-party packages.
"""

from __future__ import annotations

import argparse
import itertools
import os
import sys
import tkinter as tk
from dataclasses import dataclass
from pathlib import Path
from tkinter import filedialog, ttk

sys.path.insert(0, str(Path(__file__).resolve().parent))
import midlc  # noqa: E402
import midlc_transfers  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[2]
SKIP_DIRS = {".git", ".claude", "target", "node_modules", "__pycache__", ".venv"}


# ---------------------------------------------------------------------------
# Model
# ---------------------------------------------------------------------------


@dataclass
class Loaded:
    """One `.midl` file, parsed into an interface or carrying a parse error."""

    path: Path
    interface: midlc.Interface | None = None
    error: str = ""


@dataclass
class Node:
    kind: str  # file | interface | group | method | struct | enum
    payload: object
    path: Path | None = None


def discover(roots: list[Path]) -> list[Path]:
    found: list[Path] = []
    for root in roots:
        root = Path(root)
        if root.is_file() and root.suffix == ".midl":
            if not any(part in SKIP_DIRS for part in root.parts):
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
        if any(part in SKIP_DIRS for part in path.parts):
            continue
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
            interface = midlc.Parser(midlc.lex(text)).parse_interface()
            loaded.append(Loaded(path, interface))
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


def _matches(query: str, *texts: str) -> bool:
    if not query:
        return True
    return any(query in text.lower() for text in texts)


# ---------------------------------------------------------------------------
# UI
# ---------------------------------------------------------------------------


class MidlBrowser:
    def __init__(self, root: tk.Tk, roots: list[Path]):
        self.root = root
        self.roots = roots
        self.loaded: list[Loaded] = []
        self.nodes: dict[str, Node] = {}
        self._ids = itertools.count()
        self._build()
        self.reload()

    # -- layout ------------------------------------------------------------

    def _build(self) -> None:
        self.root.title("MIDL Browser")
        self.root.geometry("1040x660")

        toolbar = ttk.Frame(self.root, padding=(8, 5))
        toolbar.pack(side="top", fill="x")
        ttk.Button(toolbar, text="Reload", command=self.reload).pack(side="left")
        ttk.Button(toolbar, text="Open .midl", command=self.open_file).pack(side="left", padx=(6, 0))
        ttk.Button(toolbar, text="Copy", command=self.copy_detail).pack(side="left", padx=(6, 0))
        ttk.Label(toolbar, text="Filter:").pack(side="left", padx=(14, 4))
        self.query = tk.StringVar()
        entry = ttk.Entry(toolbar, textvariable=self.query, width=42)
        entry.pack(side="left")
        entry.bind("<Escape>", lambda _event: self.query.set(""))
        self.query.trace_add("write", lambda *_: self.populate())

        paned = ttk.PanedWindow(self.root, orient="horizontal")
        paned.pack(fill="both", expand=True)

        left = ttk.Frame(paned)
        paned.add(left, weight=2)
        self.tree = ttk.Treeview(left, show="tree", selectmode="browse")
        scroll = ttk.Scrollbar(left, orient="vertical", command=self.tree.yview)
        self.tree.configure(yscrollcommand=scroll.set)
        self.tree.pack(side="left", fill="both", expand=True)
        scroll.pack(side="right", fill="y")
        self.tree.bind("<<TreeviewSelect>>", self._on_select)

        right = ttk.Frame(paned)
        paned.add(right, weight=3)
        self.detail = tk.Text(right, wrap="none", padx=12, pady=10, borderwidth=0)
        detail_scroll = ttk.Scrollbar(right, orient="vertical", command=self.detail.yview)
        self.detail.configure(yscrollcommand=detail_scroll.set)
        self.detail.pack(side="left", fill="both", expand=True)
        detail_scroll.pack(side="right", fill="y")
        self._configure_tags()

        self.status = ttk.Label(self.root, anchor="w", padding=(8, 3))
        self.status.pack(side="bottom", fill="x")

    def _configure_tags(self) -> None:
        for tag, colour in (
            ("file", "#555555"),
            ("interface", "#0a58ca"),
            ("group", "#6c757d"),
            ("method", "#1b7f3b"),
            ("event", "#0f766e"),
            ("struct", "#8a5a00"),
            ("enum", "#7a2f8a"),
            ("topic", "#0b6e99"),
            ("error", "#b00020"),
        ):
            self.tree.tag_configure(tag, foreground=colour)
        base = ("TkDefaultFont", 10)
        self.detail.tag_configure("title", font=("TkDefaultFont", 15, "bold"))
        self.detail.tag_configure("section", font=("TkDefaultFont", 11, "bold"), spacing3=2)
        self.detail.tag_configure("method", foreground="#1b7f3b", font=("TkDefaultFont", 10, "bold"))
        self.detail.tag_configure("event", foreground="#0f766e", font=("TkDefaultFont", 10, "bold"))
        self.detail.tag_configure("struct", foreground="#8a5a00", font=("TkDefaultFont", 10, "bold"))
        self.detail.tag_configure("enum", foreground="#7a2f8a", font=("TkDefaultFont", 10, "bold"))
        self.detail.tag_configure("topic", foreground="#0b6e99", font=("TkDefaultFont", 10, "bold"))
        self.detail.tag_configure("meta", foreground="#6c757d")
        self.detail.tag_configure("code", font=("Consolas", 10), foreground="#202020")
        self.detail.tag_configure("doc", foreground="#444444")
        self.detail.tag_configure("error", foreground="#b00020")
        self.detail.configure(font=base)

    # -- data flow ---------------------------------------------------------

    def reload(self) -> None:
        self.loaded = load(discover(self.roots))
        self.populate()
        self._update_status()

    def open_file(self) -> None:
        chosen = filedialog.askopenfilename(
            title="Open a .midl definition",
            filetypes=[("MIDL definitions", "*.midl"), ("All files", "*.*")],
        )
        if not chosen:
            return
        self.roots.append(Path(chosen))
        self.reload()

    def populate(self) -> None:
        self.tree.delete(*self.tree.get_children())
        self.nodes.clear()
        query = self.query.get().strip().lower()
        for loaded in self.loaded:
            self._insert_file(loaded, query)

    def _insert_file(self, loaded: Loaded, query: str) -> None:
        label = self._rel(loaded.path)
        if loaded.interface is None:
            if not _matches(query, label, loaded.error):
                return
            iid = self._add("", f"{label}   (parse error)", "error")
            self.nodes[iid] = Node("file", loaded, loaded.path)
            return

        interface = loaded.interface
        if query and not _matches(query, interface.name, interface.docs, label, str(loaded.path)):
            methods = [m for m in interface.methods if _matches(query, m.name, m.doc)]
            structs = [s for s in interface.structs if _matches(query, s.name, s.doc)]
            enums = [e for e in interface.enums if _matches(query, e.name)]
            topics = [t for t in interface.topics if _matches(query, t.name, t.source, t.payload, t.doc, *t.permissions)]
            if not (methods or structs or enums or topics):
                return
            shown = midlc.Interface(interface.name, interface.docs, methods, structs, enums, topics)
        else:
            shown = interface

        file_iid = self._add("", label, "file")
        self.nodes[file_iid] = Node("file", loaded, loaded.path)
        iface_iid = self._add(file_iid, f"{interface.name}", "interface")
        self.nodes[iface_iid] = Node("interface", loaded, loaded.path)

        calls = [m for m in shown.methods if not m.oneway]
        events = [m for m in shown.methods if m.oneway]
        self._add_method_group(iface_iid, loaded, "Methods", calls)
        self._add_method_group(iface_iid, loaded, "Events", events)
        if shown.structs:
            group = self._add(iface_iid, f"Structs ({len(shown.structs)})", "group")
            self.nodes[group] = Node("group", loaded, loaded.path)
            for struct in shown.structs:
                iid = self._add(group, struct.name, "struct")
                self.nodes[iid] = Node("struct", struct, loaded.path)
        if shown.enums:
            group = self._add(iface_iid, f"Enums ({len(shown.enums)})", "group")
            self.nodes[group] = Node("group", loaded, loaded.path)
            for enum in shown.enums:
                iid = self._add(group, enum.name, "enum")
                self.nodes[iid] = Node("enum", enum, loaded.path)
        if shown.topics:
            group = self._add(iface_iid, f"Topics ({len(shown.topics)})", "group")
            self.nodes[group] = Node("group", loaded, loaded.path)
            for topic in shown.topics:
                iid = self._add(group, topic.name, "topic")
                self.nodes[iid] = Node("topic", topic, loaded.path)

    def _add_method_group(self, parent: str, loaded: Loaded, title: str, methods: list[midlc.Method]) -> None:
        if not methods:
            return
        group = self._add(parent, f"{title} ({len(methods)})", "group")
        self.nodes[group] = Node("group", loaded, loaded.path)
        for method in methods:
            text = f"{method.name}   id {method.method_id}"
            iid = self._add(group, text, kind_of(method))
            self.nodes[iid] = Node("method", method, loaded.path)

    def _add(self, parent: str, text: str, tag: str) -> str:
        iid = f"n{next(self._ids)}"
        self.tree.insert(parent, "end", iid=iid, text=text, tags=(tag,))
        return iid

    # -- rendering ---------------------------------------------------------

    def _on_select(self, _event: tk.Event) -> None:
        selection = self.tree.selection()
        if not selection:
            return
        node = self.nodes.get(selection[0])
        if node is not None:
            self._render(node)

    def _put(self, text: str, tag: str = "") -> None:
        self.detail.insert("end", text, tag)

    def _render(self, node: Node) -> None:
        self.detail.configure(state="normal", wrap="none")
        self.detail.delete("1.0", "end")
        if node.kind == "file":
            self._render_file(node)
        elif node.kind in ("interface", "group"):
            self._render_interface(node)
        elif node.kind == "method":
            self._render_method(node)
        elif node.kind == "struct":
            self._render_struct(node)
        elif node.kind == "enum":
            self._render_enum(node)
        elif node.kind == "topic":
            self._render_topic(node)
        self.detail.configure(state="disabled")
        self.detail.mark_set("insert", "1.0")

    def _render_file(self, node: Node) -> None:
        loaded: Loaded = node.payload  # type: ignore[assignment]
        self._put(f"{self._rel(loaded.path)}\n", "title")
        if loaded.interface is None:
            self._put("\nParse error\n", "error")
            self._put(f"{loaded.error}\n", "error")
        else:
            self._render_interface(node)

    def _render_interface(self, node: Node) -> None:
        loaded: Loaded = node.payload  # type: ignore[assignment]
        interface = loaded.interface
        assert interface is not None
        self._put(f"{interface.name}\n", "title")
        self._put(f"file          {self._rel(loaded.path)}\n", "meta")
        self._put(f"interface id  {interface.id:#018x}  ({interface.id})\n", "meta")
        self._put(f"rust module   {interface.module}\n", "meta")
        if interface.docs:
            self._put(f"\n{interface.docs}\n", "doc")

        calls = [m for m in interface.methods if not m.oneway]
        events = [m for m in interface.methods if m.oneway]
        self._render_method_list(calls, "Methods")
        self._render_method_list(events, "Events")

        if interface.topics:
            self._put(f"\nTopics ({len(interface.topics)})\n", "section")
            for topic in interface.topics:
                retained = " retained" if topic.retained else ""
                self._put(f"  {topic.name}\n", "topic")
                self._put(f"      payload {topic.payload}  qos {topic.qos}{retained}\n", "meta")
                if topic.source != topic.name:
                    self._put(f"      source  {topic.source}\n", "code")
                for permission in topic.permissions:
                    self._put(f"      permission {permission}\n", "meta")
                if topic.doc:
                    self._put(f"      {topic.doc}\n", "doc")

        if interface.structs:
            self._put(f"\nStructs ({len(interface.structs)})\n", "section")
            for struct in interface.structs:
                self._put(f"  {struct.name}\n", "struct")
                if struct.doc:
                    self._put(f"      {struct.doc}\n", "doc")
                for f in struct.fields:
                    self._put(f"      {f.name}: {f.ty}\n", "code")

        if interface.enums:
            self._put(f"\nEnums ({len(interface.enums)})\n", "section")
            for enum in interface.enums:
                self._put(f"  {enum.name}\n", "enum")
                self._put(f"      {', '.join(enum.variants)}\n", "code")

    def _render_method_list(self, methods: list[midlc.Method], title: str) -> None:
        if not methods:
            return
        self._put(f"\n{title} ({len(methods)})\n", "section")
        for method in methods:
            kind = kind_of(method)
            self._put(f"  {method.name}", kind)
            self._put(f"   id {method.method_id} ({method.method_id:#x})  {kind}\n", "meta")
            self._put(f"      {signature(method)}\n", "code")
            if method.doc:
                self._put(f"      {method.doc}\n", "doc")

    def _render_method(self, node: Node) -> None:
        method: midlc.Method = node.payload  # type: ignore[assignment]
        kind = kind_of(method)
        self._put(f"{kind} {method.name}\n", kind)
        self._put(f"{kind} id  {method.method_id} ({method.method_id:#x})\n", "meta")
        self._put(f"signature  {signature(method)}\n", "code")
        if method.doc:
            self._put(f"\n{method.doc}\n", "doc")
        if method.params:
            self._put(f"\nArguments ({len(method.params)})\n", "section")
            for p in method.params:
                self._put(f"  {p.name}: {p.ty}\n", "code")
        if method.returns:
            self._put(f"\nReturns ({len(method.returns)})\n", "section")
            for p in method.returns:
                self._put(f"  {p.name}: {p.ty}\n", "code")
        if method.transfers:
            self._put(f"\nTransfers ({len(method.transfers)})\n", "section")
            for t in method.transfers:
                self._put(f"  {t.name}: {midlc_transfers.describe(t)}\n", "code")

    def _render_struct(self, node: Node) -> None:
        struct: midlc.Struct = node.payload  # type: ignore[assignment]
        self._put(f"struct {struct.name}\n", "title")
        if struct.doc:
            self._put(f"\n{struct.doc}\n", "doc")
        self._put(f"\nFields ({len(struct.fields)})\n", "section")
        for f in struct.fields:
            self._put(f"  {f.name}: {f.ty}\n", "code")

    def _render_enum(self, node: Node) -> None:
        enum: midlc.Enum = node.payload  # type: ignore[assignment]
        self._put(f"enum {enum.name}\n", "title")
        self._put(f"\nVariants ({len(enum.variants)})\n", "section")
        for variant in enum.variants:
            self._put(f"  {variant}\n", "code")

    def _render_topic(self, node: Node) -> None:
        topic: midlc.Topic = node.payload  # type: ignore[assignment]
        self._put(f"topic {topic.name}\n", "title")
        self._put(f"payload     {topic.payload}\n", "meta")
        self._put(f"qos         {topic.qos}\n", "meta")
        self._put(f"retained    {'yes' if topic.retained else 'no'}\n", "meta")
        if topic.source != topic.name:
            self._put(f"source      {topic.source}\n", "code")
        for permission in topic.permissions:
            self._put(f"permission  {permission}\n", "code")
        if topic.params:
            self._put(f"\nWildcards ({len(topic.params)})\n", "section")
            for param in topic.params:
                label = param.name if param.name else "(unnamed)"
                self._put(
                    f"  {param.rust_name}  segment {param.index}  {param.kind}  {label}\n",
                    "code",
                )
        if topic.doc:
            self._put(f"\n{topic.doc}\n", "doc")

    def copy_detail(self) -> None:
        text = self.detail.get("1.0", "end-1c")
        self.root.clipboard_clear()
        self.root.clipboard_append(text)

    # -- helpers -----------------------------------------------------------

    def _rel(self, path: Path) -> str:
        try:
            return str(path.resolve().relative_to(REPO_ROOT))
        except ValueError:
            return str(path)

    def _update_status(self) -> None:
        files = len(self.loaded)
        ok = [entry for entry in self.loaded if entry.interface is not None]
        methods = sum(len(entry.interface.methods) for entry in ok)  # type: ignore[union-attr]
        structs = sum(len(entry.interface.structs) for entry in ok)  # type: ignore[union-attr]
        enums = sum(len(entry.interface.enums) for entry in ok)  # type: ignore[union-attr]
        topics = sum(len(entry.interface.topics) for entry in ok)  # type: ignore[union-attr]
        errors = files - len(ok)
        text = (
            f"{len(ok)} interface(s) in {files} file(s) · {methods} methods · "
            f"{structs} structs · {enums} enums · {topics} topics"
        )
        if errors:
            text += f" · {errors} file(s) failed to parse"
        self.status.configure(text=text)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="*", type=Path, help="files or directories to scan (default: repo root)")
    args = parser.parse_args()
    roots = args.paths or [REPO_ROOT]

    root = tk.Tk()
    MidlBrowser(root, roots)
    root.mainloop()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
