"""midlc document backend: Markdown reference and JSON manifest emitters.

Part of the Messenger IDL compiler (issue #90); see `midlc.py` for the CLI.
"""

from __future__ import annotations

from midlc_model import Interface
from midlc_rings import emit_markdown_rings, manifest_rings
from midlc_transfers import describe, signature


def emit_markdown(interface: Interface) -> str:
    lines = [f"# `{interface.name}`", "", f"Interface id: `{interface.id:#x}`", ""]
    if interface.docs:
        lines += [interface.docs, ""]
    lines += ["## Methods", "", "| Method | Id | Kind | Signature |", "|---|---|---|---|"]
    for m in interface.methods:
        args = ", ".join(f"{p.name}: {p.ty}" for p in m.params)
        rets = ", ".join(f"{p.name}: {p.ty}" for p in m.returns)
        lines.append(
            f"| {m.name} | {m.method_id} | {'oneway' if m.oneway else 'sync'} "
            f"| `({args}) -> ({rets}){signature(m)}` |"
        )
    transferring = [m for m in interface.methods if m.transfers]
    if transferring:
        lines += ["", "## Transfers", "", "Objects a request carries outside its body, in the parcel's",
                  "`handles` and `buffers` vectors.", "",
                  "| Method | Name | Slot |", "|---|---|---|"]
        for m in transferring:
            for t in m.transfers:
                lines.append(f"| {m.name} | `{t.name}` | {describe(t)} |")
    if interface.topics:
        lines += ["", "## Topics", "", "| Topic | Payload | QoS | Retained | Permissions |", "|---|---|---|---|---|"]
        for topic in interface.topics:
            retained = "yes" if topic.retained else "no"
            permissions = ", ".join(f"`{p}`" for p in topic.permissions)
            lines.append(
                f"| `{topic.name}` | `{topic.payload}` | {topic.qos} | {retained} | {permissions} |"
            )
    lines += emit_markdown_rings(interface)
    for struct in interface.structs:
        lines += ["", f"## struct `{struct.name}`", ""]
        lines += [f"- `{f.name}: {f.ty}`" for f in struct.fields]
    for enum in interface.enums:
        lines += ["", f"## enum `{enum.name}`", "", "- " + ", ".join(enum.variants)]
    return "\n".join(lines) + "\n"


def emit_manifest(interface: Interface) -> dict:
    manifest = {
        "interface": interface.name,
        "interface_id": f"{interface.id:#x}",
        "methods": [method_entry(m) for m in interface.methods],
        # Every declared topic, with both permission strings derived from the
        # pattern (issue #307): no hand-typed `publish:`/`subscribe:` strings.
        "topics": [
            {
                "name": topic.name,
                "source": topic.source,
                "payload": topic.payload,
                "qos": topic.qos,
                "retained": topic.retained,
                "publish_permission": f"publish:{topic.name}",
                "subscribe_permission": f"subscribe:{topic.name}",
            }
            for topic in interface.topics
        ],
    }
    if interface.rings:
        manifest["rings"] = manifest_rings(interface)
    return manifest


def method_entry(method) -> dict:
    """A manifest method row; `transfers` only when the request declares some."""
    entry = {"name": method.name, "id": method.method_id, "oneway": method.oneway}
    if method.transfers:
        entry["transfers"] = [transfer_entry(t) for t in method.transfers]
    return entry


def transfer_entry(transfer) -> dict:
    entry = {"name": transfer.name, "kind": transfer.kind, "slot": transfer.index}
    if transfer.kind == "channel":
        entry["interface"] = transfer.interface
    if transfer.kind == "rings":
        entry["rings"] = transfer.rings
    return entry
