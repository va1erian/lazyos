#!/usr/bin/env python3
"""The Playground's session scripts, generated (`tools/screenshot/examples/playground_*.json`).

Every click names a control the app prints with the UI probe (`UI:WIDGET ... name=<widget>`):
the tree, list and event rows are `tree_row:<n>`, `gallery_row:<n>`, `topics_row:<n>` and
`events_row:<n>`, so no step depends on a measured offset.

    python tools/playground/session.py --write    # regenerate the JSON files
    python tools/playground/session.py --check    # fail when a checked-in file is stale
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
EXAMPLES = ROOT / "tools" / "screenshot" / "examples"
WINDOW = "Messenger Playground (developer tool)"
# The Topic monitor, the Playground's second window (docs/messenger-playground-plan.md section 3).
TOPICS_WINDOW = "Messenger Topics (developer tool)"


def click(widget: str, window: str = WINDOW) -> dict:
    return {"click_at": {"window": window, "widget": widget}}


def tclick(widget: str) -> dict:
    """A click in the Topic monitor window."""
    return click(widget, TOPICS_WINDOW)


def chord(modifier: str, key: str, **extra) -> list[dict]:
    """A key with a modifier held (`key_down`, the key, `key_up`)."""
    return [{"key_down": modifier}, {"key": key, **extra}, {"key_up": modifier}]


def head() -> list[dict]:
    """Wait for the desktop, the app and its first probe lines."""
    return [
        {"wait_for": "PKGD:PROVISION:DONE", "timeout": 420},
        {"wait_for": "PLAYGROUND:UP:PASS", "timeout": 240},
        {"wait_for": "name=tree window=Messenger Playground", "timeout": 90},
        {"wait": 1.0},
    ]


def quit_steps() -> list[dict]:
    """Ctrl+Q, then a few seconds for the children to be reaped."""
    return chord("ctrl", "q", until="PLAYGROUND:QUIT:PASS", timeout=60, retries=2) + [
        {"wait": 4.0},
        {"quit": True},
    ]


def search(text: str) -> list[dict]:
    """Replace the search box's text."""
    return [click("search")] + chord("ctrl", "a") + [{"type": text}, {"wait": 1.0}]


def pick_confd_info() -> list[dict]:
    """Search the one method with a `store_dir` field and select it."""
    return search("store_dir") + [
        click("tree_row:1"),
        {"wait_for": "PLAYGROUND:OPEN:os.lazy.confd.v1", "timeout": 60},
        {"wait": 1.0},
    ]


def load_example(query: str) -> list[dict]:
    """Gallery tab, filter, select the first row, Load example."""
    return chord("ctrl", "4") + search(query) + [
        click("gallery_row:0"),
        {"wait": 1.0},
        click("btn:load"),
        {"wait": 1.0},
    ]


def listen_topic(query: str) -> list[dict]:
    """Topics tab, filter, select the first row, Listen."""
    return chord("ctrl", "3") + [{"wait": 2.0}] + search(query) + [
        click("topics_row:0"),
        {"wait": 1.0},
        click("btn:listen"),
    ]


def browse() -> list[dict]:
    steps = head() + [{"shot": "browse_initial"}] + search("store_dir") + [
        {"shot": "browse_search"},
        click("tree_row:1"),
        {"wait_for": "PLAYGROUND:OPEN:os.lazy.confd.v1", "timeout": 60},
        {"wait": 1.0},
        {"shot": "browse_method_doc"},
        click("btn:insert_call"),
        {"wait": 1.0},
        click("btn:serve"),
        {"wait": 1.0},
        {"shot": "browse_inserted"},
    ]
    steps += chord("ctrl", "2") + [{"wait": 2.5}, {"shot": "browse_live"}]
    steps += chord("ctrl", "3") + [{"wait": 3.0}, {"shot": "browse_topics"}]
    steps += chord("ctrl", "4") + [{"wait": 1.5}, {"shot": "browse_gallery"}]
    steps += [{"key": "f1"}, {"wait": 1.5}, {"shot": "browse_help"}]
    return steps + quit_steps()


def run() -> list[dict]:
    steps = head() + [click("editor")] + chord("ctrl", "a") + [
        {"type": 'print("hello from the playground");\n'},
        {"key": "f5", "until": "PLAYGROUND:RUN:1:EXIT:0:", "timeout": 90, "retries": 2},
        {"wait": 1.0},
        {"shot": "run_hello"},
    ]
    steps += chord("ctrl", "1") + pick_confd_info() + [
        click("btn:call_form"),
        {"wait": 1.0},
        {"shot": "run_form"},
        click("form_call"),
        {"wait_for": "PLAYGROUND:CALL:os.lazy.confd.v1.Info:OK", "timeout": 90},
        {"wait": 1.0},
        {"shot": "run_call_reply"},
        click("editor"),
    ]
    # A call with an argument: the form's text box takes the path.
    steps += search("confd get") + [
        click("tree_row:1"),
        {"wait": 1.0},
        click("btn:call_form"),
        {"wait": 1.0},
        {"type": "sys/ui/theme"},
        click("form_call"),
        {"wait_for": "PLAYGROUND:CALL:os.lazy.confd.v1.Get:OK", "timeout": 90},
        {"wait": 1.0},
        {"shot": "run_call_args"},
        click("editor"),
    ]
    # A call the developer-tool label refuses: the service's text is shown.
    steps += chord("ctrl", "a") + [
        {"type": "print(sys::keyd::ping());\n"},
        {"key": "f5", "until": "PLAYGROUND:RUN:4:EXIT:", "timeout": 90, "retries": 2},
        {"wait": 1.0},
        {"shot": "run_refused"},
    ]
    # A run that waits for ever, and Stop.
    steps += chord("ctrl", "a") + [
        {"type": 'print("serving");\nmsg::on("demo/pg/wait", |e| print(e.topic));\nmsg::run();\n'},
        {"key": "f5", "until": "PLAYGROUND:RUN:5:START:", "timeout": 90, "retries": 2},
        {"wait": 3.0},
        {"shot": "run_serving"},
        click("stop_button"),
        {"wait_for": "PLAYGROUND:RUN:5:EXIT:stopped:", "timeout": 60},
        {"wait": 1.0},
        {"shot": "run_stopped"},
    ]
    return steps + quit_steps()


def serve() -> list[dict]:
    """Example 12 served by run 1 and called by run 2; Stop; serve again and Quit with it live."""
    steps = head() + load_example("rhai (run 1") + [
        {"shot": "serve_loaded"},
        {"key": "f5", "until": "PLAYGROUND:RUN:1:START:", "timeout": 90, "retries": 2},
        {"wait": 3.0},
        {"shot": "serve_serving"},
    ]
    steps += load_example("rhai (run 2") + [
        {"key": "f5", "until": "PLAYGROUND:RUN:2:EXIT:", "timeout": 90, "retries": 2},
        {"wait": 1.0},
        {"shot": "serve_answered"},
    ]
    steps += chord("ctrl", "2") + [
        {"wait": 3.0},
        {"shot": "serve_live"},
        click("stop_button"),
        {"wait_for": "PLAYGROUND:RUN:1:EXIT:stopped:", "timeout": 60},
        {"wait": 3.0},
        {"shot": "serve_stopped"},
    ]
    steps += load_example("rhai (run 1") + [
        {"key": "f5", "until": "PLAYGROUND:RUN:3:START:", "timeout": 90, "retries": 2},
        {"wait": 3.0},
        {"shot": "serve_serving_again"},
    ]
    # Run 3 is still live: closing the window must end its child.
    return steps + quit_steps()


def listen() -> list[dict]:
    # Peek: the retained value without keeping a subscription (a script, run 1).
    steps = head() + chord("ctrl", "3") + [{"wait": 2.0}] + search("time/tick") + [
        click("topics_row:0"),
        {"wait": 1.0},
        click("btn:peek"),
        {"wait_for": "PLAYGROUND:RUN:1:EXIT:0:", "timeout": 90},
        {"wait": 1.0},
        {"shot": "listen_peek"},
    ]
    # Listen in the main window opens the Topic monitor, a second window that opens over it
    # (xuid cascades it), so from here on every click is in the monitor: more topics are
    # typed into its own topic box and listened with its Listen button.
    steps += listen_topic("time/tick") + [
        {"wait_for": "PLAYGROUND:TOPICS:UP", "timeout": 90},
        {"wait_for": "PLAYGROUND:LISTEN:time/tick", "timeout": 60},
        {"wait_for": "PLAYGROUND:EVENT:time/tick:", "timeout": 60},
        {"wait": 1.5},
        tclick("events_row:0"),
        {"wait": 1.0},
        {"shot": "listen_tick"},
    ]
    steps += [tclick("pub_topic")] + chord("ctrl", "a") + [
        {"type": "system/stats/memory"},
        tclick("pub_listen"),
        {"wait_for": "PLAYGROUND:LISTEN:system/stats/memory", "timeout": 60},
        {"wait_for": "PLAYGROUND:EVENT:system/stats/memory:", "timeout": 60},
        {"wait": 3.0},
        {"shot": "listen_memory"},
        tclick("pub_topic"),
    ] + chord("ctrl", "a") + [
        {"type": "demo/pg/x"},
        tclick("pub_listen"),
        {"wait_for": "PLAYGROUND:LISTEN:demo/pg/x", "timeout": 60},
        tclick("pub_payload"),
        {"type": "hello from publish"},
        # Publish asks the main window to run the one-line script (run 2 there).
        tclick("pub_go"),
        {"wait_for": "PLAYGROUND:EVENT:demo/pg/x:", "timeout": 90},
        {"wait": 1.0},
        tclick("events_row:0"),
        {"wait": 1.0},
        {"shot": "listen_published"},
        # The tab's controls: pin the event, clear the list (the pin stays), copy, save, pause.
        tclick("pin_button"),
        tclick("clear_button"),
        tclick("copy_button"),
        tclick("save_button"),
        tclick("pause_button"),
        {"wait": 1.5},
        {"shot": "listen_controls"},
    ]
    # Ctrl+Q in the monitor quits the whole tool: the monitor goes first (TOPICS:QUIT), then
    # the main window.
    return steps + quit_steps()


def elevd() -> list[dict]:
    """Example 11b asks `elevd` to restart `inputd`: the trusted prompt names the tool, an
    administrator approves, the service restarts (docs/accounts-plan.md U2)."""
    steps = head() + load_example("run 2: trigger") + [
        {"key": "f5", "until": "XUID:PROMPT:UP", "timeout": 120, "retries": 1},
        {"wait": 2.0},
        {"shot": "elevd_prompt"},
        {"type": "admin"},
        {"key": "tab"},
        {"type": "nimda"},
        {"key": "ret", "until": "outcome=granted", "timeout": 120},
        {"wait_for": "init: started inputd (pid", "timeout": 120},
        {"wait_for": "PLAYGROUND:RUN:1:EXIT:", "timeout": 120},
        {"wait": 3.0},
        {"shot": "elevd_done"},
    ]
    return steps + quit_steps()


def review() -> list[dict]:
    """The strip, a pasted script that asks before its first Run, nothing runs on open."""
    steps = head() + [{"wait": 4.0}, {"shot": "review_idle"}]
    # Text the editor never saw typed: written in the search box, copied from there and pasted
    # over the editor's buffer (pasting back text the user typed in the editor is a restore, not
    # an arrival, so it would not ask).
    steps += [click("search")] + chord("ctrl", "a") + [{"type": "print(40 + 2);"}]
    steps += chord("ctrl", "a") + chord("ctrl", "c") + [click("editor")]
    steps += chord("ctrl", "a") + chord("ctrl", "v") + [
        {"wait": 1.0},
        {"key": "f5"},
        {"wait": 2.0},
        {"shot": "review_card"},
        {"key": "esc"},
        {"wait": 1.0},
        {"shot": "review_cancelled"},
        {"key": "f5"},
        {"wait": 2.0},
        {"key": "enter", "until": "PLAYGROUND:RUN:1:EXIT:0:", "timeout": 90, "retries": 1},
        {"wait": 1.0},
        {"shot": "review_ran"},
        click("editor"),
        {"key": "f5", "until": "PLAYGROUND:RUN:2:EXIT:0:", "timeout": 90, "retries": 2},
        {"wait": 1.0},
        {"shot": "review_second_run"},
    ]
    # Save the buffer under a name, open it again: a file asks before its first Run.
    steps += chord("ctrl", "s") + [{"wait": 1.5}, {"shot": "review_save_dialog"}]
    steps += chord("ctrl", "a") + [{"type": "pg-test"}, {"key": "enter"}, {"wait": 1.5}, {"shot": "review_saved"}]
    steps += chord("ctrl", "o") + [{"wait": 1.5}, {"shot": "review_open_dialog"}]
    steps += chord("ctrl", "a") + [{"type": "pg-test.rhai"}, {"key": "enter"}, {"wait": 1.5},
                                   click("editor"), {"key": "f5"}, {"wait": 2.0},
                                   {"shot": "review_file_card"}, {"key": "esc"}, {"wait": 1.0}]
    return steps + quit_steps()


def notice() -> list[dict]:
    """The strip's "x" hides the band and the layout reclaims the space. The choice is stored, so
    this session ends with the notice dismissed on purpose: `notice_stored` is the next boot and
    puts it back (the other sessions need the strip, so run these two last and in this order)."""
    steps = head() + [{"wait": 3.0}, {"shot": "notice_shown"}, click("devtool:strip_dismiss"),
                      {"wait_for": "PLAYGROUND:NOTICE:HIDDEN", "timeout": 60},
                      {"wait": 2.0},
                      {"shot": "notice_hidden"}]
    return steps + quit_steps()


def notice_stored() -> list[dict]:
    """A later launch finds the choice (`NOTICE:HIDDEN:stored`, no strip), then View > Show
    developer-tool notice (Ctrl+Shift+D) brings the band back and forgets the choice."""
    steps = [
        {"wait_for": "PKGD:PROVISION:DONE", "timeout": 420},
        {"wait_for": "PLAYGROUND:UP:PASS", "timeout": 240},
        {"wait_for": "PLAYGROUND:NOTICE:HIDDEN:stored", "timeout": 90},
        {"wait_for": "name=tree window=Messenger Playground", "timeout": 90},
        {"wait": 3.0},
        {"shot": "notice_stored_hidden"},
        {"key_down": "ctrl"}, {"key_down": "shift"}, {"key": "d"}, {"key_up": "shift"}, {"key_up": "ctrl"},
        {"wait_for": "PLAYGROUND:NOTICE:SHOWN", "timeout": 60},
        {"wait": 2.0},
        {"shot": "notice_stored_shown"},
    ]
    return steps + quit_steps()


def drag_divider(widget: str, dx: int, dy: int, shot: str) -> list[dict]:
    """Press on a divider (a probe name), pull it by (`dx`, `dy`) in a few paced moves, release."""
    steps = [{"move_to": {"window": WINDOW, "widget": widget}}, {"wait": 0.3}, {"mouse_down": "left"}, {"wait": 0.1}]
    for _ in range(4):
        steps.append({"mouse_move": [dx // 4, dy // 4]})
    return steps + [{"wait": 0.1}, {"mouse_up": "left"}, {"wait": 1.5}, {"shot": shot}]


def layout() -> list[dict]:
    """Ctrl+J hides the editor and the Output (the doc pane takes the column), the button brings
    them back, the dividers are dragged, the row is hidden again and F5 shows it (a run reveals
    the Output). Nothing runs because of a toggle: the only run is the one F5 starts."""
    steps = head() + [{"shot": "layout_initial"}]
    steps += chord("ctrl", "j") + [
        {"wait_for": "PLAYGROUND:LAYOUT:BOTTOM:HIDDEN", "timeout": 60},
        {"wait": 1.5},
        {"shot": "layout_hidden"},
        click("toggle_bottom"),
        {"wait_for": "PLAYGROUND:LAYOUT:BOTTOM:SHOWN", "timeout": 60},
        {"wait": 1.5},
        {"shot": "layout_shown"},
    ]
    steps += drag_divider("split_main", -120, 0, "layout_explorer_narrow")
    steps += drag_divider("split_doc", 0, -100, "layout_doc_short")
    # A pause longer than the 2 s registry tick: the dragged layout is written to the volume.
    steps += [{"wait": 4.0}]
    steps += chord("ctrl", "j") + [
        {"wait_for": "PLAYGROUND:LAYOUT:BOTTOM:HIDDEN", "timeout": 60},
        {"wait": 1.0},
        {"key": "f5", "until": "PLAYGROUND:RUN:1:START:", "timeout": 90, "retries": 2},
        {"wait_for": "PLAYGROUND:RUN:1:EXIT:0:", "timeout": 90},
        {"wait": 1.5},
        {"shot": "layout_run_reveals"},
    ]
    return steps + quit_steps()


def maximize() -> list[dict]:
    """A double click on the title bar maximizes the window: the doc pane and the editor/Output
    take the extra room and the Explorer keeps its width (an image with a 1920x1080 display, see
    `maximize_run.py`); a second one restores it."""
    bar = {"move_to": {"window": WINDOW, "widget": "devtool:strip", "offset": [100, -56]}}
    steps = head() + [{"wait": 1.0}, {"shot": "max_before"}, bar, {"wait": 0.3},
                      {"mouse_click": "left"}, {"wait": 0.1}, {"mouse_click": "left"},
                      {"wait": 3.0}, {"shot": "max_after"},
                      click("tree_row:1"), {"wait": 1.0}, {"shot": "max_page"}, bar, {"wait": 0.3},
                      {"mouse_click": "left"}, {"wait": 0.1}, {"mouse_click": "left"},
                      {"wait": 3.0}, {"shot": "max_restored"}]
    return steps + quit_steps()


def layout_stored() -> list[dict]:
    """A later launch finds the dragged dividers (`LAYOUT:LOADED`); the divider of a hidden row
    is pulled up to show it again."""
    steps = [
        {"wait_for": "PKGD:PROVISION:DONE", "timeout": 420},
        {"wait_for": "PLAYGROUND:UP:PASS", "timeout": 240},
        {"wait_for": "PLAYGROUND:LAYOUT:LOADED", "timeout": 90},
        {"wait_for": "name=tree window=Messenger Playground", "timeout": 90},
        {"wait": 2.0},
        {"shot": "layout_stored"},
    ]
    steps += chord("ctrl", "j") + [
        {"wait_for": "PLAYGROUND:LAYOUT:BOTTOM:HIDDEN", "timeout": 60},
        {"wait": 1.5},
    ]
    steps += drag_divider("split_doc", 0, -120, "layout_pulled")
    steps += [{"wait_for": "PLAYGROUND:LAYOUT:BOTTOM:SHOWN", "timeout": 60}]
    return steps + quit_steps()


SESSIONS = {
    "playground_browse.json": browse,
    "playground_run.json": run,
    "playground_serve.json": serve,
    "playground_listen.json": listen,
    "playground_elevd.json": elevd,
    "playground_review.json": review,
    "playground_notice.json": notice,
    "playground_notice_stored.json": notice_stored,
    "playground_layout.json": layout,
    "playground_layout_stored.json": layout_stored,
    "playground_maximize.json": maximize,
}


def text_of(build) -> str:
    return json.dumps(build(), indent=1) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--write", action="store_true")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args(argv)
    stale = []
    for name, build in SESSIONS.items():
        path = EXAMPLES / name
        text = text_of(build)
        if args.write:
            path.write_text(text)
        elif args.check and (not path.is_file() or path.read_text() != text):
            stale.append(name)
    for name in stale:
        print(f"{name} is stale: run python tools/playground/session.py --write", file=sys.stderr)
    return 1 if stale else 0


if __name__ == "__main__":
    raise SystemExit(main())
