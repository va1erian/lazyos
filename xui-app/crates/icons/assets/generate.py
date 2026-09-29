#!/usr/bin/env python3
"""Regenerates `src/data/` from the vendored SVGs in `assets/svg/`.

Run from anywhere: `python crates/xui-icons/assets/generate.py`.

Each SVG is a flat list of `rect`/`circle`/`ellipse`/`path` elements styled with
the class vocabulary in the file's own `<style>` block. This script resolves
every element to a single filled and/or stroked path of `PathSeg`s (rects,
circles and SVG arcs become cubic Béziers) and maps colours to `Tone`s, so the
crate needs no SVG parser at run time. Standard library only.
"""

import math
import re
import xml.etree.ElementTree as ET
from pathlib import Path

HERE = Path(__file__).resolve().parent
SRC = HERE.parent / "src" / "data"

# Category -> icons, in gallery order.
CATEGORIES = {
    "hardware": ["computer", "monitor", "keyboard", "mouse", "hard-disk", "floppy",
                 "cd-rom", "printer", "modem", "network", "server", "speaker"],
    "files": ["folder", "folder-open", "document", "image", "music", "archive"],
    "system": ["trash", "trash-full", "settings", "search", "home", "clock", "mail",
               "lock", "help", "warning", "info", "error"],
    "toolkit": ["rpc", "pub-sub", "theme", "widget", "terminal", "window"],
}

FILL_CLASSES = {"c": "Cream", "t": "Teal", "m": "Rose", "a": "Amber", "b": "Cobalt",
                "r": "Clay", "g": "Lavender", "k": "Ink", "s": "Screen", "l": "Mint"}
STROKE_CLASSES = {"o": ("Ink", 1.5), "h": ("Shine", 1.2), "h2": ("Ink", 1.0),
                  "zz": ("Rose", 1.6), "zt": ("Teal", 1.6), "zc": ("Cream", 1.6)}
HEX_TONES = {"#0b0822": "Ink", "#f1e3bd": "Cream", "#19b5a5": "Teal", "#e2366f": "Rose",
             "#f4a81d": "Amber", "#4162e0": "Cobalt", "#d4562f": "Clay",
             "#9a93d0": "Lavender", "#1b1552": "Screen", "#9fe6dd": "Mint",
             "#221a66": "Night", "#fff6dc": "Shine", "#fff2d0": "Chalk"}

NUMBER = re.compile(r"[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?")
ARGS = {"M": 2, "L": 2, "H": 1, "V": 1, "C": 6, "S": 4, "A": 7, "Z": 0}


def arc_to_cubics(x1, y1, rx, ry, phi, large, sweep, x2, y2):
    """SVG arc endpoint -> centre parameterisation -> cubic segments (F.6.5)."""
    if rx == 0 or ry == 0 or (x1, y1) == (x2, y2):
        return [("L", x2, y2)]
    rx, ry = abs(rx), abs(ry)
    cp, sp = math.cos(math.radians(phi)), math.sin(math.radians(phi))
    dx, dy = (x1 - x2) / 2, (y1 - y2) / 2
    x1p, y1p = cp * dx + sp * dy, -sp * dx + cp * dy
    lam = x1p ** 2 / rx ** 2 + y1p ** 2 / ry ** 2
    if lam > 1:
        rx, ry = rx * math.sqrt(lam), ry * math.sqrt(lam)
    num = rx ** 2 * ry ** 2 - rx ** 2 * y1p ** 2 - ry ** 2 * x1p ** 2
    den = rx ** 2 * y1p ** 2 + ry ** 2 * x1p ** 2
    co = math.sqrt(max(0.0, num / den)) * (-1 if large == sweep else 1)
    cxp, cyp = co * rx * y1p / ry, -co * ry * x1p / rx
    cx = cp * cxp - sp * cyp + (x1 + x2) / 2
    cy = sp * cxp + cp * cyp + (y1 + y2) / 2

    def angle(ux, uy, vx, vy):
        a = math.atan2(ux * vy - uy * vx, ux * vx + uy * vy)
        return a

    t1 = angle(1, 0, (x1p - cxp) / rx, (y1p - cyp) / ry)
    dt = angle((x1p - cxp) / rx, (y1p - cyp) / ry, (-x1p - cxp) / rx, (-y1p - cyp) / ry)
    if not sweep and dt > 0:
        dt -= 2 * math.pi
    elif sweep and dt < 0:
        dt += 2 * math.pi
    n = max(1, math.ceil(abs(dt) / (math.pi / 2) - 1e-9))
    step = dt / n
    k = 4 / 3 * math.tan(step / 4)
    out = []
    t = t1
    for _ in range(n):
        c1, s1, c2, s2 = math.cos(t), math.sin(t), math.cos(t + step), math.sin(t + step)
        pts = [(c1 - k * s1, s1 + k * c1), (c2 + k * s2, s2 - k * c2), (c2, s2)]
        vals = []
        for ex, ey in pts:
            ex, ey = ex * rx, ey * ry
            vals += [cp * ex - sp * ey + cx, sp * ex + cp * ey + cy]
        out.append(("C", *vals))
        t += step
    out[-1] = ("C", *out[-1][1:5], x2, y2)
    return out


def parse_path(d):
    """Path data -> absolute ("M"|"L"|"C"|"Z", ...) segments."""
    tokens = re.findall(r"[MmLlHhVvCcSsAaZz]|" + NUMBER.pattern, d)
    segs, i = [], 0
    cx = cy = sx = sy = 0.0
    last_c2 = None
    cmd = None
    while i < len(tokens):
        if tokens[i].isalpha():
            cmd = tokens[i]
            i += 1
            if cmd in "Zz":
                segs.append(("Z",))
                cx, cy = sx, sy
                last_c2 = None
                continue
        up, rel = cmd.upper(), cmd.islower()
        a = [float(t) for t in tokens[i:i + ARGS[up]]]
        i += ARGS[up]
        ox, oy = (cx, cy) if rel else (0.0, 0.0)
        if up == "M":
            cx, cy = a[0] + ox, a[1] + oy
            sx, sy = cx, cy
            segs.append(("M", cx, cy))
            cmd = "l" if rel else "L"
            last_c2 = None
        elif up == "L":
            cx, cy = a[0] + ox, a[1] + oy
            segs.append(("L", cx, cy))
            last_c2 = None
        elif up == "H":
            cx = a[0] + ox
            segs.append(("L", cx, cy))
            last_c2 = None
        elif up == "V":
            cy = a[0] + oy
            segs.append(("L", cx, cy))
            last_c2 = None
        elif up in "CS":
            if up == "C":
                x1, y1 = a[0] + ox, a[1] + oy
                x2, y2, x, y = a[2] + ox, a[3] + oy, a[4] + ox, a[5] + oy
            else:
                x1, y1 = (2 * cx - last_c2[0], 2 * cy - last_c2[1]) if last_c2 else (cx, cy)
                x2, y2, x, y = a[0] + ox, a[1] + oy, a[2] + ox, a[3] + oy
            segs.append(("C", x1, y1, x2, y2, x, y))
            last_c2 = (x2, y2)
            cx, cy = x, y
        elif up == "A":
            x, y = a[5] + ox, a[6] + oy
            segs += arc_to_cubics(cx, cy, a[0], a[1], a[2], int(a[3]), int(a[4]), x, y)
            cx, cy = x, y
            last_c2 = None
    return segs


def shape_path(el):
    tag = el.tag.split("}")[1]
    f = lambda k, default=0.0: float(el.get(k, default))
    if tag == "path":
        return parse_path(el.get("d"))
    if tag == "rect":
        x, y, w, h = f("x"), f("y"), f("width"), f("height")
        r = min(f("rx"), w / 2, h / 2)
        if r == 0:
            return parse_path(f"M{x} {y}H{x + w}V{y + h}H{x}Z")
        return parse_path(
            f"M{x + r} {y}H{x + w - r}A{r} {r} 0 0 1 {x + w} {y + r}V{y + h - r}"
            f"A{r} {r} 0 0 1 {x + w - r} {y + h}H{x + r}A{r} {r} 0 0 1 {x} {y + h - r}"
            f"V{y + r}A{r} {r} 0 0 1 {x + r} {y}Z")
    if tag in ("circle", "ellipse"):
        cx, cy = f("cx"), f("cy")
        rx, ry = (f("r"), f("r")) if tag == "circle" else (f("rx"), f("ry"))
        return parse_path(
            f"M{cx - rx} {cy}A{rx} {ry} 0 0 1 {cx + rx} {cy}A{rx} {ry} 0 0 1 {cx - rx} {cy}Z")
    raise ValueError(f"unsupported element <{tag}>")


def tone(value):
    return None if value in (None, "none") else HEX_TONES[value.lower()]


def style(el, inherited_fill):
    classes = set((el.get("class") or "").split())
    fill = inherited_fill
    for c in classes:
        if c in FILL_CLASSES:
            fill = FILL_CLASSES[c]
    if "n" in classes:
        fill = None
    if el.get("fill") is not None:
        fill = tone(el.get("fill"))
    stroke = None
    for c in classes:
        if c in STROKE_CLASSES:
            stroke = STROKE_CLASSES[c]
    if el.get("stroke") is not None:
        width = float(el.get("stroke-width", stroke[1] if stroke else 1.0))
        stroke = (tone(el.get("stroke")), width)
    return fill, stroke


def collect(node, inherited_fill=None):
    for el in node:
        tag = el.tag.split("}")[1]
        if tag == "style":
            continue
        if tag == "g":
            fill, _ = style(el, None)
            yield from collect(el, fill)
            continue
        fill, stroke = style(el, inherited_fill)
        yield shape_path(el), fill, stroke


def num(v):
    s = f"{v:.2f}".rstrip("0").rstrip(".")
    if s in ("-0", ""):
        s = "0"
    return s if "." in s else s + ".0"


def rust_path(segs):
    parts = []
    for s in segs:
        if s[0] == "Z":
            parts.append("PathSeg::Close")
        else:
            name = {"M": "MoveTo", "L": "LineTo", "C": "CubicTo"}[s[0]]
            parts.append(f"PathSeg::{name}({', '.join(num(v) for v in s[1:])})")
    return "&[" + ", ".join(parts) + "]"


def variant(name):
    return "".join(p.capitalize() for p in name.split("-"))


def const_name(name):
    return name.upper().replace("-", "_")


def emit_shape(path, fill, stroke):
    p = rust_path(path)
    if fill and stroke:
        return f"Shape::both({p}, Tone::{fill}, Tone::{stroke[0]}, {num(stroke[1])})"
    if fill:
        return f"Shape::fill({p}, Tone::{fill})"
    if stroke:
        return f"Shape::line({p}, Tone::{stroke[0]}, {num(stroke[1])})"
    raise ValueError("shape with neither fill nor stroke")


def main():
    SRC.mkdir(parents=True, exist_ok=True)
    header = ("// @generated by crates/xui-icons/assets/generate.py from assets/svg/*.svg.\n"
              "// Do not edit by hand.\n\n")
    for category, names in CATEGORIES.items():
        out = [header, "use xui_core::backend::PathSeg;\n\n",
               "use crate::shape::Shape;\nuse crate::tone::Tone;\n\n"]
        for name in names:
            root = ET.parse(HERE / "svg" / f"{name}.svg").getroot()
            out.append(f"pub(super) const {const_name(name)}: &[Shape] = &[\n")
            for path, fill, stroke in collect(root):
                out.append(f"    {emit_shape(path, fill, stroke)},\n")
            out.append("];\n\n")
        (SRC / f"{category}.rs").write_text("".join(out).rstrip("\n") + "\n", encoding="utf-8")

    mod = [header]
    for category in sorted(CATEGORIES):
        mod.append("#[rustfmt::skip]\n"
                   "#[allow(clippy::approx_constant)] // generated coordinates, not maths\n"
                   f"mod {category};\n")
    mod.append("\nuse crate::shape::Shape;\n\n")
    mod.append("/// The groups the set is organised into.\n"
               "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\npub enum Category {\n")
    docs = {"hardware": "Computer hardware and peripherals.",
            "files": "Folders and file types.",
            "system": "Everyday operating-system icons.",
            "toolkit": "Concepts specific to the xui toolkit."}
    for category in CATEGORIES:
        mod.append(f"    /// {docs[category]}\n    {category.capitalize()},\n")
    mod.append("}\n\n")
    mod.append("/// One icon of the Global Village set.\n"
               "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\npub enum Village {\n")
    allnames = [(c, n) for c, ns in CATEGORIES.items() for n in ns]
    for _, name in allnames:
        mod.append(f"    /// The `{name}` icon.\n    {variant(name)},\n")
    mod.append("}\n\nimpl Village {\n")
    mod.append(f"    /// Every icon, in gallery order.\n    pub const ALL: &'static [Village] = &[\n")
    for _, name in allnames:
        mod.append(f"        Village::{variant(name)},\n")
    mod.append("    ];\n\n")
    for fn, doc, rtype, arm in (
        ("name", "The icon's file stem: `\"hard-disk\"`, `\"pub-sub\"`.", "&'static str",
         lambda c, n: f'"{n}"'),
        ("category", "The group the icon belongs to.", "Category",
         lambda c, n: f"Category::{c.capitalize()}"),
        ("shapes", "The icon's shapes, back to front, on a 32-unit grid.", "&'static [Shape]",
         lambda c, n: f"{c}::{const_name(n)}"),
    ):
        mod.append(f"    /// {doc}\n    pub fn {fn}(self) -> {rtype} {{\n        match self {{\n")
        for c, n in allnames:
            mod.append(f"            Village::{variant(n)} => {arm(c, n)},\n")
        mod.append("        }\n    }\n\n")
    mod[-1] = mod[-1].rstrip("\n") + "\n}\n"
    (SRC / "mod.rs").write_text("".join(mod), encoding="utf-8")


if __name__ == "__main__":
    main()
