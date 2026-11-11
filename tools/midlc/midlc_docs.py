"""midlc document backend: Markdown reference and JSON manifest emitters.

Part of the Messenger IDL compiler (issue #90); see `midlc.py` for the CLI.
"""

from __future__ import annotations

from midlc_model import Interface
from midlc_rings import emit_markdown_rings, manifest_rings
from midlc_objects import describe, type_text


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
            f"| `({args}) -> ({rets})` |"
        )
    lines += markdown_services(interface)
    lines += markdown_method_attrs(interface)
    lines += markdown_errors(interface)
    carrying = [m for m in interface.methods if m.objects]
    if carrying:
        lines += ["", "## Objects", "", "Kernel objects a request carries, in the order of the parcel's",
                  "object list (the index each field must hold).", "",
                  "| Method | Field | Type | Object |", "|---|---|---|---|"]
        for m in carrying:
            for o in m.objects:
                lines.append(f"| {m.name} | `{o.dotted}` | `{type_text(o)}` | {describe(o)} |")
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


def markdown_services(interface: Interface) -> list[str]:
    if not interface.services:
        return []
    lines = ["", "## Service", ""]
    for service in interface.services:
        others = [n for n in service.interfaces if n != interface.name]
        also = f" (together with {', '.join(f'`{n}`' for n in others)})" if others else ""
        kind = "a family, one name per instance" if service.instanced else "registered once"
        lines.append(f"- Served under the registry name `{service.name}`{also}; {kind}.")
    return lines


def markdown_method_attrs(interface: Interface) -> list[str]:
    """Methods that declare a scope or a deadline."""
    declared = [m for m in interface.methods if m.scope is not None or m.deadline_ms is not None]
    if not declared:
        return []
    lines = ["", "## Scopes and deadlines", "", "| Method | Scope interface | Scope id | Deadline |", "|---|---|---|---|"]
    for m in declared:
        scope = f"`{m.scope}`" if m.scope is not None else "this interface"
        scope_id = f"`{m.scope_id:#x}`" if m.scope is not None else "-"
        deadline = f"{m.deadline_ms} ms" if m.deadline_ms is not None else "-"
        lines.append(f"| {m.name} | {scope} | {scope_id} | {deadline} |")
    return lines


def markdown_errors(interface: Interface) -> list[str]:
    if not interface.errors:
        return []
    lines = ["", "## Errors", "", "Carried in the reply's standard error field (id 15).", "",
             "| Code | Name | Meaning |", "|---|---|---|"]
    for error in interface.errors:
        meaning = " ".join(error.doc.split()) if error.doc else ""
        lines.append(f"| {error.code} | {error.name} | {meaning} |")
    return lines


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
    # The registry names that serve the interface, its error codes and (on a
    # method) its scope id and deadline: only when declared, so an interface
    # that states none keeps its entry.
    if interface.services:
        manifest["services"] = [s.name for s in interface.services]
    if interface.errors:
        manifest["errors"] = [{"name": e.name, "code": e.code} for e in interface.errors]
    return manifest


def method_entry(method) -> dict:
    """A manifest method row; `objects` only when the request carries some."""
    entry = {"name": method.name, "id": method.method_id, "oneway": method.oneway}
    if method.scope is not None:
        entry["scope"] = method.scope
        entry["scope_id"] = f"{method.scope_id:#x}"
    if method.deadline_ms is not None:
        entry["deadline_ms"] = method.deadline_ms
    if method.objects:
        entry["objects"] = [object_entry(o) for o in method.objects]
    return entry


def object_entry(obj) -> dict:
    """One object: its field name, kind and index, the channel's interface or
    the buffer's rings, and the field path when it sits inside a struct."""
    entry = {"name": obj.name, "kind": obj.kind, "index": obj.index}
    if obj.kind == "channel":
        entry["interface"] = obj.interface
    if obj.kind == "rings":
        entry["rings"] = obj.rings
    if obj.nested:
        entry["path"] = obj.path
    return entry
