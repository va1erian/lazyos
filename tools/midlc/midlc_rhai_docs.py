"""midlc Rhai API backend: the reference page (`libs/rhai-lazy/api/README.md`).

Part of the Messenger IDL compiler; the modules themselves come from
`midlc_rhai`. The page is generated from the same data, so it lists exactly
the functions a script has, with each method's IDL signature and the first
line of its documentation.
"""

from __future__ import annotations

from midlc_model import Interface, snake_case
from midlc_rhai import (
    GENERATED,
    alias,
    callable_methods,
    param_name,
    placeholder_names,
    signature,
    topic_helpers,
)

INTRO = """\
# Messenger APIs for Rhai scripts (`sys::*`)

<!-- GENERATED -->

Every Messenger interface in `idl/` is a Rhai module under `sys::`, generated
by `midlc --rhai-api` (see `docs/lazyrad-messenger-plan.md`). The modules are
available in the `rhai` command and in every LazyRAD form script on LazyOS:

```rhai
let theme = sys::confd::get("sys/ui/theme");          // call a method
let value = sys::confd::new_value();                  // build a struct
sys::confd::on_changed("sys/ui/#", |event| {          // react to a topic
    print(event.payload.path);
});
```

Each function is one call into the `msg` module, so values, errors and
timeouts are exactly as [`docs/rhai/msg.md`](../../../docs/rhai/msg.md)
describes: a struct is an object map, an enum a variant name, a refusal a
catchable error with the service's own text. The `.rhai` file of each module
is next to this page and is the authoritative reference.

For every declared topic a module has `<NAME>_PATTERN` (the filter),
`<name>_topic(...)` (a concrete name, when the topic has placeholders),
`on_<name>(..., handler)` (subscribe and run `handler(event)` for each event),
`subscribe_<name>(...)` (pull with `sub.next()`) and `publish_<name>(..., payload)`.
In a LazyRAD form, `on_*` handlers run on the form's window; in the `rhai`
command, after `msg::run()`.

Kernel ACL scopes (interfaces no service receives) have no module.
"""


def module_section(interface: Interface) -> list[str]:
    name = alias(interface)
    lines = [f"## `sys::{name}`", "", f"Interface `{interface.name}`, source [`{name}.rhai`]({name}.rhai)."]
    if interface.docs:
        lines += ["", interface.docs.splitlines()[0]]
    lines += ["", "| Function | IDL | About |", "|---|---|---|"]
    for method in callable_methods(interface):
        params = ", ".join(param_name(p.name) for p in method.params)
        about = method.doc.splitlines()[0] if method.doc else ""
        lines.append(f"| `{snake_case(method.name)}({params})` | `{signature(method)}` | {about} |")
    for struct in interface.structs:
        lines.append(f"| `new_{snake_case(struct.name)}()` | struct `{struct.name}` | a `{struct.name}` at its zero value |")
    skipped = [m.name for m in interface.methods if m.objects]
    if skipped:
        lines += ["", "Not callable from a script (the request carries a kernel object): "
                  + ", ".join(f"`{m}`" for m in skipped) + "."]
    if interface.enums:
        lines += [""]
        for enum in interface.enums:
            prefix = snake_case(enum.name).upper()
            lines.append(
                f"- `{prefix}` = the `{enum.name}` variants; "
                + ", ".join(f"`{prefix}_{snake_case(v).upper()}`" for v in enum.variants)
            )
    helpers = topic_helpers(interface)
    if helpers:
        lines += ["", "| Topic | Payload | Helpers |", "|---|---|---|"]
        for topic, stem in helpers:
            args = ", ".join(placeholder_names(topic))
            lead = f"{args}, " if args else ""
            calls = [f"`on_{stem}({lead}handler)`", f"`subscribe_{stem}({args})`", f"`publish_{stem}({lead}payload)`"]
            if args:
                calls.insert(0, f"`{stem}_topic({args})`")
            lines.append(f"| `{topic.source}` | `{topic.payload}` | {', '.join(calls)} |")
    return lines + [""]


def emit_reference(interfaces: list[Interface]) -> str:
    """The whole reference page."""
    generated = " ".join(line.removeprefix("// ") for line in GENERATED.strip().splitlines())
    lines = [INTRO.replace("GENERATED", generated), "| Module | Interface |", "|---|---|"]
    lines += [f"| [`sys::{alias(i)}`](#sys{alias(i)}) | `{i.name}` |" for i in interfaces]
    lines.append("")
    for interface in interfaces:
        lines += module_section(interface)
    return "\n".join(lines).rstrip("\n") + "\n"
