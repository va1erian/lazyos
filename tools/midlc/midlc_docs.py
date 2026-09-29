"""midlc document backend: Markdown reference and JSON manifest emitters.

Part of the Messenger IDL compiler (issue #90); see `midlc.py` for the CLI.
"""

from __future__ import annotations

from midlc_model import Interface


def emit_markdown(interface: Interface) -> str:
    lines = [f"# `{interface.name}`", "", f"Interface id: `{interface.id:#x}`", ""]
    if interface.docs:
        lines += [interface.docs, ""]
    lines += ["## Methods", "", "| Method | Id | Kind | Signature |", "|---|---|---|---|"]
    for m in interface.methods:
        args = ", ".join(f"{p.name}: {p.ty}" for p in m.params)
        rets = ", ".join(f"{p.name}: {p.ty}" for p in m.returns)
        lines.append(f"| {m.name} | {m.method_id} | {'oneway' if m.oneway else 'sync'} | `({args}) -> ({rets})` |")
    for struct in interface.structs:
        lines += ["", f"## struct `{struct.name}`", ""]
        lines += [f"- `{f.name}: {f.ty}`" for f in struct.fields]
    for enum in interface.enums:
        lines += ["", f"## enum `{enum.name}`", "", "- " + ", ".join(enum.variants)]
    return "\n".join(lines) + "\n"


def emit_manifest(interface: Interface) -> dict:
    return {
        "interface": interface.name,
        "interface_id": f"{interface.id:#x}",
        "methods": [{"name": m.name, "id": m.method_id, "oneway": m.oneway} for m in interface.methods],
    }
