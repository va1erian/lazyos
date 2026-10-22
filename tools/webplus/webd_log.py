"""Reading webd's part of a LazyWeb+ run's serial log (`judge.py` re-exports all of it).

LazyWeb+ is a plain xui app and a client of the resident daemon `webd` (os.lazy.webd), which owns WebKit and
its two processes. A log therefore holds the client's markers (`WEBPLUS:*`) and webd's:

* the engine's own markers, which webd relays as `WEBD:NOTE:<line>` (`xui-app/webd/src/lines.rs`): what follows
  the prefix is the line the engine's C++ half prints (`WEBPROCESS:UP:<pid>`, `PROCESS:LAUNCH:web:<pid>`,
  `INFO:PAINT:...`, `FAIL:...`). `engine_lines` is the one reader; no harness matches an engine marker under
  a literal `WEBPLUS:` prefix (that was the in-process engine's form);
* webd's own: `WEBD:UP:PASS`, `WEBD:VIEW:CREATE client=<label> view=<id>`, `WEBD:VIEW:DESTROY view=<id>`,
  `WEBD:LOST view=<id> reason=<refused|crashed|memory|other|unknown>`.

Pure text in, values out.
"""

from __future__ import annotations

import re

#: The client's marker prefix.
P = "WEBPLUS"
#: The prefix of an engine marker, which webd relays.
NOTE = "WEBD:NOTE:"
#: webd's own markers.
WEBD_UP = "WEBD:UP:PASS"
#: The package label of the client, as webd names the caller of a view.
CLIENT_LABEL = "app:os.lazy.lazywebplus"
VIEW_CREATE = re.compile(r"WEBD:VIEW:CREATE client=(?P<client>\S+) view=(?P<view>\d+)")
VIEW_DESTROY = re.compile(r"WEBD:VIEW:DESTROY view=(?P<view>\d+)")
VIEW_LOST = re.compile(r"WEBD:LOST view=(?P<view>\d+) reason=(?P<reason>\w+)")
VIEW_EVENT = re.compile(r"WEBD:VIEW:(?:CREATE client=(?P<client>\S+) view=(?P<made>\d+)|DESTROY view=(?P<gone>\d+))")


def lines(text: str, prefix: str) -> list[str]:
    """What follows `prefix` on every line that has it (anywhere: the console may interleave)."""
    found = []
    for line in text.splitlines():
        at = line.find(prefix)
        if at >= 0:
            found.append(line[at + len(prefix):].strip())
    return found


def engine_line(line: str) -> str | None:
    """The engine's marker on one serial line, without webd's `WEBD:NOTE:` prefix (anywhere on the line: the
    console may interleave), or None when the line is not one."""
    at = line.find(NOTE)
    return line[at + len(NOTE):].strip() if at >= 0 else None


def engine_lines(text: str) -> list[str]:
    """The engine's markers of a log, in order, without webd's `WEBD:NOTE:` prefix: webd relays what the
    WebKit half prints (`PROCESS:LAUNCH:web:41`, `WEBPROCESS:UP:41`, `INFO:PAINT:...`). Every reader of an
    engine marker goes through here; the client's own markers stay `judge.markers(text, ...)`."""
    return lines(text, NOTE)


def engine_text(text: str) -> str:
    """`engine_lines` as one text, a marker a line, for the regular expressions that read several."""
    return "".join(f"{line}\n" for line in engine_lines(text))


def engine_markers(text: str, kind: str) -> list[str]:
    """The text after `<kind>:` of every engine marker that starts with it."""
    return [line[len(kind) + 1:] for line in engine_lines(text) if line.startswith(f"{kind}:")]


def engine_pids(text: str, kind: str, what: str) -> list[str]:
    """The pids of every engine `<kind>:<what>:<pid>...` marker, in order."""
    return re.findall(rf"^{kind}:{what}:(\d+)", engine_text(text), re.M)


def client_views(text: str) -> tuple[list[str], list[str]]:
    """(the views webd made for the client, in order; those of them still live at the end of the log). A view
    id may be used again once destroyed, so the log is followed in order."""
    made: list[str] = []
    live: list[str] = []
    for match in VIEW_EVENT.finditer(text):
        if match["made"] and match["client"] == CLIENT_LABEL:
            made.append(match["made"])
            live.append(match["made"])
        elif match["gone"] and match["gone"] in live:
            live.remove(match["gone"])
    return made, live


PROCESS_EVENT = re.compile(
    r"^(?:PROCESS:(?P<kind>LAUNCH|EXIT):(?P<ptype>\w+):(?P<ppid>\d+)(?::(?P<rest>[^\r\n]*))?"
    r"|PROCESS:TERMINATE:(?P<ttype>\w+):(?P<tpid>\d+):group=(?P<group>-?\d+):pid=(?P<single>-?\d+))", re.M)


def process_history(text: str, must_end: tuple[str, ...] = ("web",)) -> tuple[list[str], set[tuple[str, str]]]:
    """Follows every engine process through the log in order (issue #862): a launch makes it live, an
    exit ends it. LazyOS ids are task slots and are reused at once, so an id means a process only between
    its launch and its end: an exit of an id that is not live, or of another type than the launch, a
    launch of an id still live, a `terminate` that sent nothing to a live process (neither the group's
    kill nor the process's), and a process of a type in `must_end` still live at the end are all
    problems. The page's process (`web`) ends with its view; the network process belongs to webd, which
    outlives the window, so it is not asked to end. Returns the problems and the (type, id) pairs seen
    to end."""
    problems: list[str] = []
    live: dict[str, str] = {}
    ended: set[tuple[str, str]] = set()
    for match in PROCESS_EVENT.finditer(engine_text(text)):
        if match["kind"] == "LAUNCH":
            kind, pid = match["ptype"], match["ppid"]
            if pid in live:
                problems.append(f"the {kind} process {pid} was launched while the {live[pid]} process {pid} was still live")
            live[pid] = kind
            ended.discard((kind, pid))
        elif match["kind"] == "EXIT":
            kind, pid = match["ptype"], match["ppid"]
            if pid not in live:
                problems.append(f"an exit of the {kind} process {pid}, which was not live")
            elif live[pid] != kind:
                problems.append(f"an exit of the {kind} process {pid}, which was launched as {live.pop(pid)}")
            else:
                del live[pid]
                ended.add((kind, pid))
        elif match["ttype"]:
            kind, pid = match["ttype"], match["tpid"]
            if pid in live and match["group"] != "0" and match["single"] != "0":
                problems.append(f"WebKit's terminate of the {kind} process {pid} sent no signal "
                                f"(group errno {match['group']}, pid errno {match['single']})")
    for pid, kind in live.items():
        if kind in must_end:
            problems.append(f"the {kind} process {pid} was launched and never seen to end")
    return problems, ended
