#!/usr/bin/env python3
"""Scaffold a new LazyOS desktop app in one command.

    python tools/xui/new_app.py notes --name "Notes" --description "Quick notes"

A desktop app is not done until every front end knows it (AGENTS.md: the
image build, `run_demo.py`, the GUI launcher, and a core package with its
icons). This writes all of it at once:

* ``xui-app/src/bin/<short>.rs``: a starting app on xui layouts that already
  prints ``<MARKER>:UP:PASS`` and ``<MARKER>:QUIT:PASS``;
* the ``[[bin]]`` in ``xui-app/Cargo.toml`` and the elf in
  ``tools/xui/build.py``, ``build_support/xui_embed.rs`` (shipped with every
  desktop image) and ``tools/run_demo.py``;
* the core package: ``xui-app/packages/<short>/`` (manifest, README and
  icons), its entry in ``tools/xui/core_packages.py`` and
  ``build_support/core_packages.rs``, and its art in the ``app-icons`` crate
  (a placeholder to replace);
* the GUI launcher's viewer and session (``tools/lazygui/catalog.py``,
  ``tools/screenshot/examples/xui_<short>.json``) and the core-apps launch
  session.

What it cannot know is left as follow-ups it prints: deriving the package
permissions from a traced run, the icon's art, and the app itself.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
# The manifest validator's categories, so a bad one is refused up front.
sys.path.insert(0, str(ROOT / "tools" / "pkg"))
import pkgmanifest  # noqa: E402


class ScaffoldError(Exception):
    """A registry did not have the shape the scaffolder edits."""


@dataclass(frozen=True)
class App:
    """The new app's names."""

    short: str
    name: str
    description: str
    category: str

    @property
    def marker(self) -> str:
        """The serial-evidence prefix: `NOTES` for `notes`."""
        return self.short.upper().replace("-", "_")

    @property
    def type_name(self) -> str:
        """The app struct's name: `NoteTaker` for `note-taker`."""
        return "".join(part.capitalize() for part in re.split(r"[-_]", self.short))

    @property
    def elf(self) -> str:
        return f"xui-{self.short}.elf"

    @property
    def system_name(self) -> str:
        return f"os.lazy.{self.short}"


def insert_before_close(text: str, opener: str, close: str, line: str) -> str:
    """Insert `line` as the last item of the collection that starts at `opener`
    and ends at the first later line reading `close`, indented one level
    deeper than that closing line."""
    start = text.find(opener)
    if start < 0:
        raise ScaffoldError(f"missing {opener!r}")
    at = text.find("\n", start) + 1
    while 0 < at < len(text):
        end = text.find("\n", at)
        end = len(text) if end < 0 else end
        current = text[at:end]
        if current.strip() == close:
            indent = current[: len(current) - len(current.lstrip())] + "    "
            item = "\n".join(indent + part if part else part for part in line.split("\n"))
            return text[:at] + item + "\n" + text[at:]
        at = end + 1
    raise ScaffoldError(f"no {close!r} after {opener!r}")


def noted(app: App, comment: str, line: str) -> str:
    """`line` under a comment naming the app, so a new last item never reads
    as part of the comment block above it (e.g. "LazyWeb, only with ...")."""
    return f"{comment} {app.name}: {app.description.rstrip('.')}.\n{line}"


def read(root: Path, rel: str) -> tuple[str, bool]:
    """`rel`'s text with plain line ends, and whether it used CRLF."""
    raw = (root / rel).read_bytes().decode("utf-8")
    return raw.replace("\r\n", "\n"), "\r\n" in raw



def quoted(value: str) -> str:
    """`value` as a double-quoted literal that is valid Rust, TOML (a basic
    string) and Python alike, whatever quotes or backslashes it holds.
    Control characters are refused before this (`main`): JSON's `\\u00XX`
    escape is not one Rust accepts."""
    return json.dumps(value, ensure_ascii=False)


def app_source(app: App) -> str:
    """The starting app: a layout, a handle, a message, and the evidence."""
    m, t = app.marker, app.type_name
    return f'''//! `{app.short}`: {app.description}
//!
//! Serial evidence: `{m}:UP:PASS` after the first frame and `{m}:QUIT:PASS` on
//! `q` (or the window close button).

use xui_app::launch;
use xui_core::prelude::*;

/// The window size when a compositor lays the app out; as the display owner
/// it fills the screen instead.
const WINDOW: (i32, i32) = (480, 320);

#[derive(Clone)]
enum Msg {{
    Clicked,
    Quit,
}}

#[derive(Default)]
struct {t} {{
    status: Handle<Label<Msg>>,
    clicks: u32,
}}

impl App for {t} {{
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {{
        match msg {{
            Msg::Clicked => {{
                self.clicks += 1;
                let text = format!("Clicked {{}} time(s)", self.clicks);
                self.status.get().set_text(&text);
            }}
            Msg::Quit => {{
                println!("{m}:QUIT:PASS");
                ui.quit();
            }}
        }}
    }}
}}

fn main() {{
    launch::run("{m}", {quoted(app.name)}, WINDOW, |ui, backend| {{
        backend.on_first_frame(|| println!("{m}:UP:PASS"));
        let app = {t}::default();
        ui.root(
            column().padding(16).gap(8).children((
                label({quoted(app.name)}).title(),
                label({quoted(app.description)}).bind(&app.status).fill(1),
                row()
                    .justify(Align::End)
                    .child(button("Click me").on_click(Msg::Clicked)),
            )),
        )?;
        ui.on_key(|key, _| (key == Key::Q).then_some(Msg::Quit));
        ui.on_close(|| Some(Msg::Quit));
        Ok(app)
    }})
}}
'''


def manifest(app: App) -> str:
    return f'''# {app.name}: a core LazyOS app, packaged by `tools/xui/build.py`
# (scaffolded by `tools/xui/new_app.py`).
#
# `bin/{app.short}.elf` is not checked in: the build copies the built xui app
# into a scratch copy of this tree. `version` is replaced by the workspace
# version and `autostart` is set from `LAZYOS_XUI_AUTOSTART`.

[app]
name = {quoted(app.name)}
system_name = "{app.system_name}"
author = "LazyOS"
version = "0.1.0"
description = {quoted(app.description)}
category = {quoted(app.category)}

[entry]
binary = "bin/{app.short}.elf"
# A desktop app is a client of the `xuid` compositor, not the display owner.
args = ["--client"]
# A static musl program: `init` starts it under the Linux ABI personality.
abi = "linux"
autostart = false

[permissions]
# The display and input every window needs. Derive the rest from a run under
# `LAZYOS_LABEL_TRACE=1` (AGENTS.md) once the app does more.
interfaces = ["os.lazy.display.v1", "os.lazy.input.v1"]
'''


def session(app: App) -> str:
    steps = [
        {"wait_for": f"{app.marker}:UP:PASS", "timeout": 240},
        {"wait": 2.0},
        {"shot": f"{app.short}_initial"},
        {"key": "q", "until": f"{app.marker}:QUIT:PASS", "timeout": 60, "retries": 2},
        {"quit": True},
    ]
    return "[\n" + ",\n".join("  " + json.dumps(step) for step in steps) + "\n]\n"


def scaffold(root: Path, app: App) -> list[str]:
    """Write the app and register it everywhere; returns the files touched."""
    # Every file is worked out first and written only when all of them are,
    # so a registry with an unexpected shape leaves the tree untouched.
    writes: dict[str, tuple[str, bool]] = {}

    def new(rel: str, text: str) -> None:
        if (root / rel).exists():
            raise ScaffoldError(f"{rel} already exists")
        writes[rel] = (text, False)

    def change(rel: str, fn) -> None:
        text, crlf = read(root, rel)
        writes[rel] = (fn(text), crlf)

    new(f"xui-app/src/bin/{app.short}.rs", app_source(app))
    new(f"xui-app/packages/{app.short}/manifest.toml", manifest(app))
    new(
        f"xui-app/packages/{app.short}/docs/README.md",
        f"# {app.name}\n\n{app.description}.\n",
    )
    new(f"tools/screenshot/examples/xui_{app.short}.json", session(app))

    def cargo(text: str) -> str:
        block = f'[[bin]]\nname = "xui-{app.short}"\npath = "src/bin/{app.short}.rs"\n'
        last = text.rfind("[[bin]]")
        if last < 0:
            raise ScaffoldError("no [[bin]] in xui-app/Cargo.toml")
        end = text.find("\n\n", last)
        end = len(text) if end < 0 else end + 1
        return text[:end] + "\n" + block + text[end:]

    change("xui-app/Cargo.toml", cargo)
    change(
        "tools/xui/build.py",
        lambda t: insert_before_close(
            t, "BINS = {", "}", noted(app, "#", f'"xui-{app.short}": "{app.elf}",')
        ),
    )
    change(
        "build_support/xui_embed.rs",
        lambda t: insert_before_close(
            t, "const DOCUMENT_XUI_APPS: &[&str] = &[", "];", noted(app, "//", f'"{app.elf}",')
        ),
    )
    change(
        "build_support/core_packages.rs",
        lambda t: insert_before_close(
            t, "const CORE: &[&str] = &[", "];", noted(app, "//", f'"{app.short}",')
        ),
    )
    change(
        "tools/xui/core_packages.py",
        lambda t: insert_before_close(
            t,
            "CORE_APPS: dict[str, CoreApp] = {",
            "}",
            noted(app, "#", f'"{app.short}": xui_app("{app.elf}", "{app.short}"),'),
        ),
    )

    def run_demo(text: str) -> str:
        opener = "DESKTOP_ELFS = ["
        start = text.find(opener)
        end = text.find("\n)]", start)
        if start < 0 or end < 0:
            raise ScaffoldError("no DESKTOP_ELFS list in tools/run_demo.py")
        return text[:end] + f'\n    "{app.elf}",' + text[end:]

    change("tools/run_demo.py", run_demo)

    def catalog(text: str) -> str:
        text = insert_before_close(
            text,
            "SCRIPTS = [",
            "]",
            f'("xui_{app.short}.json", {quoted("XUI app: " + app.name)}, ("desktop",), "{app.short}"),',
        )
        viewers = re.search(r"XUI_VIEWERS = \[[^\]]*\]", text)
        if viewers is None:
            raise ScaffoldError("no XUI_VIEWERS in tools/lazygui/catalog.py")
        listed = viewers.group(0)
        return text.replace(listed, listed[:-1] + f', "{app.short}"]', 1)

    change("tools/lazygui/catalog.py", catalog)

    def core_apps(text: str) -> str:
        launch = re.search(r'("type": "clear; rhai /tmp/l\.rhai [^"]*)"', text)
        if launch is None:
            raise ScaffoldError("no launch line in core_apps.json")
        text = text.replace(launch.group(0), launch.group(1) + f' {app.system_name}"', 1)
        waits = list(re.finditer(r'  \{"wait_for": "INIT:LAUNCH:PASS app=[^\n]*\n', text))
        if not waits:
            raise ScaffoldError("no launch waits in core_apps.json")
        at = waits[-1].end()
        wait = (
            f'  {{"wait_for": "INIT:LAUNCH:PASS app={app.system_name}", "timeout": 300}},\n'
        )
        return text[:at] + wait + text[at:]

    change("tools/screenshot/examples/core_apps.json", core_apps)
    change(
        "xui-app/crates/app-icons/src/lib.rs",
        lambda t: insert_before_close(
            t,
            "pub const PACKAGES: &[(&str, Art)] = &[",
            "];",
            f"// {app.name}: scaffolded with a placeholder; pick its art.\n"
            f'(\n    "xui-app/packages/{app.short}",\n'
            f"    Art::Lucide(Lucide::AppWindow, Tone::Teal),\n),",
        ),
    )
    for rel, (text, crlf) in writes.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        if crlf:
            text = text.replace("\n", "\r\n")
        path.write_bytes(text.encode("utf-8"))
    return list(writes)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("short", help="short id: lower-case letters (e.g. notes)")
    parser.add_argument("--name", help="display name (default: the short id, capitalised)")
    parser.add_argument("--description", default="A LazyOS desktop app")
    parser.add_argument("--category", default=pkgmanifest.DEFAULT_CATEGORY,
                        choices=pkgmanifest.CATEGORIES)
    parser.add_argument("--no-icons", action="store_true",
                        help="skip drawing the icons (`cargo run -p app-icons`)")
    parser.add_argument("--root", type=Path, default=ROOT, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    # The image build matches core stems as plain words (`core_packages.rs`).
    if not re.fullmatch(r"[a-z]+", args.short):
        parser.error("the short id is lower-case letters only")
    # The name and description also land in comments and Markdown lines, and
    # a control character has no escape that Rust, TOML and Python all read.
    for text in (args.name or "", args.description):
        if any(ord(char) < 0x20 or ord(char) == 0x7F for char in text):
            parser.error("names and descriptions are one line of printable text")
    app = App(
        short=args.short,
        name=args.name or args.short.capitalize(),
        description=args.description,
        category=args.category,
    )
    try:
        touched = scaffold(args.root, app)
    except ScaffoldError as error:
        print(f"new_app: {error}", file=sys.stderr)
        return 1
    # Flushed, so the list comes before cargo's output when stdout is a pipe.
    print("\n".join(f"  {path}" for path in touched), flush=True)
    if not args.no_icons:
        # A package without its three icons fails the core package build.
        drawn = subprocess.run(
            ["cargo", "run", "--quiet", "--manifest-path",
             str(args.root / "xui-app" / "Cargo.toml"), "-p", "app-icons",
             "--", f"xui-app/packages/{app.short}"],
            cwd=args.root,
        )
        if drawn.returncode != 0:
            print("new_app: drawing the icons failed; run `cargo run -p app-icons`",
                  file=sys.stderr)
            return 1
    print(
        f"""
Next:
  python tools/xui/build.py && python tools/run_demo.py --desktop
  python tools/screenshot/qemu_session.py --image target/lazyos.img \\
      --out shots/{app.short} --script tools/screenshot/examples/xui_{app.short}.json
Then derive the package permissions from a LAZYOS_LABEL_TRACE=1 run."""
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
