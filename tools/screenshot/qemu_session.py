#!/usr/bin/env python3
"""Drive a headless QEMU guest with scripted input and timed screenshots.

This lets an agent *interact* with LazyOS (type commands, click, scroll) and
capture the resulting pixels — the automated counterpart to a human at the
keyboard. Boots QEMU with ``-display none`` and talks to it over QMP.

Input is injected with the QMP ``input-send-event`` command. Keyboard uses a US
layout; mouse uses relative motion/buttons (PS/2) by default. For absolute
pointer positioning add ``--tablet`` (attaches ``usb-tablet`` on its own xHCI; the guest must
enumerate USB).

Script format (JSON)
--------------------
A list of steps, each with an optional ``at`` (seconds since boot, or since
the latest ``wait_for`` gate; see below) and exactly one action. Steps without
``at`` run immediately after the previous one.

    [
      {"at": 2.0, "shot": "boot"},
      {"at": 3.0, "type": "hello world"},
      {"at": 3.5, "key": "enter"},
      {"at": 4.0, "shot": "after_enter"},
      {"at": 5.0, "mouse_move": [200, 0]},
      {"at": 5.2, "mouse_click": "left"},
      {"at": 5.5, "mouse_scroll": 3},
      {"at": 6.0, "shot": "after_click"},
      {"at": 7.0, "quit": true}
    ]

Actions: ``shot`` (name), ``type`` (string; optional ``delay`` per character, seconds), ``key`` (name), ``keys`` (list),
``key_down`` / ``key_up`` (name; separate transitions, so a caller can hold a
modifier across steps, e.g. Alt+Tab or Ctrl+Esc), ``mouse_move`` ([dx, dy]),
``mouse_click`` (left|middle|right),
``mouse_down`` / ``mouse_up`` (left|middle|right; separate transitions, so a
caller can hold a button across steps, e.g. through a drag & drop),
``mouse_scroll`` (int), ``mouse_abs`` ([x, y]), ``click_at`` / ``move_to``
(a pixel or a named target; moves the pointer there, and clicks; see below), ``wait``
(seconds), ``wait_for`` (serial marker), ``qmp`` (a raw QMP command with
optional ``args``, e.g. hot-plugging a device:
``{"qmp": "device_add", "args": {"driver": "usb-kbd", "id": "kbd"}}``), ``quit``.

Clicking by position or name (issue #538)
-----------------------------------------
``{"click_at": {"window": "MOD Player", "widget": "play_button"}}`` clicks a
control by name (``[x, y]``, ``{"menu": ...}`` and more in
``session_pointer.py``, from an image built with ``LAZYOS_UI_PROBE=1``);
``move_to`` only moves. ``--tablet`` jumps there, else it moves from the corner.

Readiness gating
----------------
Fixed ``at`` offsets are fragile: a guest under TCG on a busy CI runner can
boot tens of seconds later than on a desktop with WHPX/KVM, so an input fired
"at 95 s" may land before the app is listening, or be handled after the
session has already quit. Gate on what the guest *reports* instead:

    {"wait_for": "SYSMON:UP:PASS", "timeout": 240}

blocks until the serial log contains that text (a plain substring; add
``"regex": true`` for a regular expression) and fails the session if it does
not appear within ``timeout`` seconds (default ``--wait-timeout``). Add
``"occurrence": N`` to wait for the *N-th* match instead of the first, so a
marker printed once per launch (e.g. a second ``EDITOR:UP:PASS``) can be gated
on; it must be an integer ``>= 1`` and defaults to ``1``. Any input
action can carry ``until`` to confirm the guest handled it, re-sending the
input when it did not (a dropped keystroke or click under load):

    {"key": "r", "until": "SYSMON:REFRESH:PASS", "timeout": 60, "retries": 2}

``until`` looks only at serial output written *after* the input was sent, so a
marker already printed by an earlier step does not count. ``timeout`` is per
attempt; ``retries`` is the number of re-sends after the first (default 0).

A ``wait_for`` gate can also *capture* part of the marker for later steps:
``"capture": "<name>"`` stores the regex's first group (or the whole match)
from the matched line, and a later ``type``, ``wait_for`` or ``until``
substitutes ``${<name>}`` (a captured value used in a regex marker is inserted
as is, so capture digits or other regex-safe text). That is
how a script kills a process whose pid only the guest knows:

    {"wait_for": "INIT:LAUNCH:PASS app=lazyshell pid=(\\d+)", "regex": true,
     "capture": "shell_pid"},
    {"type": "kill -9 ${shell_pid}"}

Once a ``wait_for`` gate is satisfied, later ``at`` values count from that
moment instead of from boot, so a timed choreography (e.g. a sequence of
relative mouse moves that cannot be retried piecemeal) keeps its internal
spacing but starts only when the guest is ready.

When a gate times out, or a ``--fail-on`` pattern shows up in the serial log,
the session captures ``shot_failed.png``, prints the serial tail, records the
failing step in ``summary.json`` and exits 1. ``summary.json`` also carries a
``timeline`` (seconds since QMP connected for every step), which shows how
close a run came to its timeouts.

Usage
-----
    python tools/screenshot/qemu_session.py --image target/lazyos.img \
        --out shots --script tools/screenshot/examples/type_and_shot.json
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from pathlib import Path

from qemu_qmp import (DEFAULT_MEMORY, Qmp, accel_args, add_data_disk_option, add_home_disk_option,
                      build_qemu_command, existing_data_disk, existing_home_disk,
                      TCG_KEY_INTERVAL, find_qemu, free_port, resolve_accel)
from session_hang import capture_hang_state
from session_pointer import (POINTER, TABLET_MAX, StepFailed, load_targets,  # noqa: F401
                             parse_screen, point, resolve_click_at)

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "net"))
import qemu_net  # noqa: E402

_ACTIONS = {
    "shot", "type", "key", "keys", "key_down", "key_up", "mouse_move",
    "mouse_click", "mouse_down", "mouse_up", "mouse_scroll", "mouse_abs",
    "click_at", "move_to", "wait", "wait_for", "qmp", "quit",
}
# Actions that send input and so may carry an `until` confirmation.
_INPUT_ACTIONS = {
    "type", "key", "keys", "key_down", "key_up", "mouse_move", "mouse_click",
    "mouse_down", "mouse_up", "mouse_scroll", "mouse_abs", "click_at", "move_to",
}
_POLL_SECONDS = 0.25


class SerialLog:
    """Incremental reader over QEMU's ``-serial file:`` output."""

    def __init__(self, path: Path, fail_on: list[str]):
        self.path = path
        self.fail_on = [re.compile(pattern) for pattern in fail_on]

    def text(self) -> str:
        try:
            return self.path.read_bytes().decode("utf-8", errors="replace")
        except OSError:
            return ""

    def size(self) -> int:
        return len(self.text())

    def check_failures(self, text: str) -> None:
        for pattern in self.fail_on:
            match = pattern.search(text)
            if match:
                line = text[text.rfind("\n", 0, match.start()) + 1:].split("\n", 1)[0]
                raise StepFailed(f"--fail-on {pattern.pattern!r} matched: {line.strip()}")

    def wait_for(self, marker: str, timeout: float, regex: bool = False,
                 since: int = 0, occurrence: int = 1) -> float:
        """Block until the ``occurrence``-th ``marker`` appears at/after ``since``.

        Returns the seconds waited; raises :class:`StepFailed` on timeout or
        when a ``--fail-on`` pattern shows up first. ``occurrence`` defaults to
        the first match, so an ``until`` gate (which passes ``since``) is
        unchanged; a caller waits for a later launch by asking for the N-th
        match. Matches are counted non-overlapping from ``since``.
        """
        pattern = re.compile(marker if regex else re.escape(marker))
        begun = time.time()
        deadline = begun + timeout
        while True:
            text = self.text()
            if sum(1 for _ in pattern.finditer(text, since)) >= occurrence:
                return time.time() - begun
            self.check_failures(text)
            if time.time() >= deadline:
                waited = repr(marker) if occurrence == 1 else f"{marker!r} occurrence {occurrence}"
                raise StepFailed(
                    f"timed out after {timeout:g}s waiting for {waited} on serial"
                )
            time.sleep(_POLL_SECONDS)

    def nth_match(self, marker: str, regex: bool = False, since: int = 0,
                  occurrence: int = 1) -> re.Match | None:
        """The ``occurrence``-th match of ``marker`` at/after ``since``, if any."""
        pattern = re.compile(marker if regex else re.escape(marker))
        for count, match in enumerate(pattern.finditer(self.text(), since), 1):
            if count == occurrence:
                return match
        return None

    def tail(self, lines: int = 40) -> str:
        return "\n".join(self.text().splitlines()[-lines:])


def perform(qmp: Qmp, action: str, step: dict, serial: SerialLog | None = None,
            timeout: float = 30.0) -> None:
    """Send one input action to the guest (``serial`` resolves probe names)."""
    if action == "type":
        # `"delay"`: seconds between characters (default 0.01); a busy TCG
        # host drops keys at the default pace.
        qmp.type_text(step["type"], delay=float(step.get("delay", 0.01)))
    elif action == "key":
        qmp.press_key(step["key"])
    elif action == "keys":
        for name in step["keys"]:
            qmp.press_key(name)
    elif action == "key_down":
        qmp.key_down(step["key_down"])
    elif action == "key_up":
        qmp.key_up(step["key_up"])
    elif action == "mouse_move":
        dx, dy = step["mouse_move"]
        qmp.mouse_move(dx, dy)
    elif action == "mouse_down":
        qmp.mouse_down(step["mouse_down"])
    elif action == "mouse_up":
        qmp.mouse_up(step["mouse_up"])
    elif action == "mouse_click":
        qmp.mouse_click(step["mouse_click"])
    elif action == "mouse_scroll":
        qmp.mouse_scroll(step["mouse_scroll"])
    elif action == "mouse_abs":
        x, y = step["mouse_abs"]
        qmp.mouse_abs(x, y)
    elif action in ("click_at", "move_to"):
        point(qmp, step, action, serial.text if serial else (lambda: ""), timeout)


_VARIABLE = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}")


def substitute(text: str, variables: dict[str, str], index: int) -> str:
    """Replace each ``${name}`` in ``text`` with a captured value."""
    def value(match: re.Match) -> str:
        name = match.group(1)
        if name not in variables:
            raise SystemExit(f"step {index}: ${{{name}}} was never captured")
        return variables[name]
    return _VARIABLE.sub(value, text)


def captured(match: re.Match) -> str:
    """What a ``capture`` stores: the first group, or the whole match."""
    return match.group(1) if match.re.groups else match.group(0)


def run_steps(qmp: Qmp, steps: list[dict], out_dir: Path, started: float,
              serial: SerialLog | None = None, wait_timeout: float = 240.0,
              timeline: list[dict] | None = None) -> list[str]:
    screenshots: list[str] = []
    if serial is None:
        serial = SerialLog(out_dir / "serial.log", [])
    if timeline is None:
        timeline = []
    # `at` counts from boot until a `wait_for` gate is satisfied, then from the
    # moment of the latest gate, so a timed choreography starts from readiness.
    origin = started
    # Values `wait_for` gates captured, for `${name}` in later `type` steps.
    variables: dict[str, str] = {}
    for index, step in enumerate(steps):
        if "at" in step:
            remaining = float(step["at"]) - (time.time() - origin)
            if remaining > 0:
                time.sleep(remaining)

        action = next((key for key in step if key in _ACTIONS), None)
        if action is None:
            raise SystemExit(f"step {index} has no recognised action: {step}")
        if "until" in step and action not in _INPUT_ACTIONS:
            raise SystemExit(f"step {index}: 'until' only applies to input actions: {step}")
        capture = step.get("capture")
        if capture is not None and (action != "wait_for" or not isinstance(capture, str)
                                    or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", capture)):
            raise SystemExit(f"step {index}: 'capture' must name a variable on a wait_for: {step}")
        for key in ("type", "wait_for", "until"):
            if isinstance(step.get(key), str):
                step = {**step, key: substitute(step[key], variables, index)}

        # A --fail-on marker (an app's FAIL line, a kernel panic) ends the
        # session now rather than after every remaining gate times out.
        serial.check_failures(serial.text())
        entry: dict = {"step": index, "action": action, "t": round(time.time() - started, 2)}
        timeline.append(entry)
        timeout = float(step.get("timeout", wait_timeout))

        if action in _INPUT_ACTIONS:
            until = step.get("until")
            attempts = 1 + int(step.get("retries", 0)) if until else 1
            for attempt in range(attempts):
                since = serial.size()
                try:
                    perform(qmp, action, step, serial, timeout)
                except StepFailed as failure:  # an unresolvable pointer target
                    raise StepFailed(f"step {index} ({action}): {failure}") from None
                if not until:
                    break
                try:
                    serial.wait_for(until, timeout, bool(step.get("regex")), since)
                except StepFailed as failure:
                    if attempt + 1 == attempts or "--fail-on" in str(failure):
                        raise StepFailed(
                            f"step {index} ({action}): {failure} "
                            f"after {attempts} attempt(s)"
                        ) from None
                    print(f"step {index}: {until!r} not seen, re-sending "
                          f"({attempt + 2}/{attempts})", flush=True)
                    continue
                entry["attempts"] = attempt + 1
                entry["confirmed"] = round(time.time() - started, 2)
                print(f"[{entry['confirmed']:7.2f}s] {until} (after {action}, "
                      f"attempt {attempt + 1})", flush=True)
                break
            continue

        if action == "wait_for":
            occurrence = step.get("occurrence", 1)
            if isinstance(occurrence, bool) or not isinstance(occurrence, int) or occurrence < 1:
                raise SystemExit(
                    f"step {index}: 'occurrence' must be an integer >= 1: {step}"
                )
            try:
                serial.wait_for(step["wait_for"], timeout, bool(step.get("regex")),
                                occurrence=occurrence)
            except StepFailed as failure:
                raise StepFailed(f"step {index} (wait_for): {failure}") from None
            if capture:
                match = serial.nth_match(step["wait_for"], bool(step.get("regex")),
                                         occurrence=occurrence)
                variables[capture] = captured(match) if match else ""
                entry["captured"] = {capture: variables[capture]}
            origin = time.time()
            entry["seen"] = round(origin - started, 2)
            print(f"[{entry['seen']:7.2f}s] {step['wait_for']}", flush=True)
        elif action == "shot":
            shot = qmp.screenshot(out_dir / f"shot_{step['shot']}")
            screenshots.append(shot.name)
            print(f"[{entry['t']:7.2f}s] captured {shot}", flush=True)
        elif action == "wait":
            time.sleep(float(step["wait"]))
        elif action == "qmp":
            qmp.execute(step["qmp"], **step.get("args", {}))
            print(f"[{entry['t']:7.2f}s] qmp {step['qmp']} {step.get('args', {})}", flush=True)
        elif action == "quit":
            try:
                qmp.execute("quit")
            except Exception:
                pass
            return screenshots
    return screenshots


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--image", help="raw disk image to boot (omit for firmware only)")
    parser.add_argument("--script", required=True, help="path to a JSON step script")
    parser.add_argument("--out", default="shots", help="output directory (default: shots)")
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--timeout", type=float, default=180.0, help="QMP/overall timeout")
    parser.add_argument("--memory", default=DEFAULT_MEMORY,
                        help="guest RAM (default: %(default)s)")
    parser.add_argument("--tablet", action="store_true",
                        help="attach a usb-tablet for absolute pointer positioning")
    parser.add_argument("--screen", default="1280x720",
                        help="WxH of the guest screen, for click_at pixels (default: %(default)s)")
    parser.add_argument("--targets", help="JSON {name: [x, y]} of click_at/move_to target names")
    parser.add_argument("--accel", default="auto",
                        choices=["auto", "none", "tcg", "whpx", "kvm"],
                        help="QEMU accelerator (auto: whpx/kvm if available)")
    parser.add_argument("--extra-arg", action="append", default=[], metavar="ARG",
                        help="extra QEMU argument; repeat for multiple")
    parser.add_argument("--wait-timeout", type=float, default=240.0,
                        help="default timeout for wait_for/until gates (seconds)")
    parser.add_argument("--fail-on", action="append", default=[], metavar="REGEX",
                        help="abort the session when the serial log matches REGEX; "
                             "repeat for multiple")
    add_data_disk_option(parser)
    add_home_disk_option(parser)
    qemu_net.add_net_options(parser, "attach a virtio-net card on QEMU's user network "
                             "(boot an image built with LAZYOS_NETD=1)")
    args = parser.parse_args()
    try:
        net_extra, _forwards = qemu_net.args_from_options(args)
    except ValueError as error:
        parser.error(str(error))
    data_disk = existing_data_disk(args.data_disk)
    home_disk = existing_home_disk(args.home_disk)

    steps = json.loads(Path(args.script).read_text(encoding="utf-8"))
    if not isinstance(steps, list):
        sys.exit("--script must contain a JSON list of steps")

    qemu = find_qemu(args.qemu)
    out_dir = Path(args.out).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    serial_log = out_dir / "serial.log"

    image = None
    if args.image:
        image_path = Path(args.image).resolve()
        if not image_path.is_file():
            sys.exit(f"--image not found: {image_path}")
        image = str(image_path)

    try:
        POINTER["screen"] = parse_screen(args.screen)
        POINTER["targets"] = load_targets(args.targets) if args.targets else {}
    except (ValueError, OSError) as error:
        parser.error(str(error))
    POINTER["tablet"] = args.tablet

    extra = list(args.extra_arg)
    if args.tablet:
        extra += ["-device", "qemu-xhci,id=tabletbus", "-device", "usb-tablet,bus=tabletbus.0"]
    extra += net_extra
    extra += accel_args(args.accel, qemu)

    port = free_port()
    command = build_qemu_command(qemu, image, port, serial_log, args.memory, extra,
                                 data_disk, home_disk=home_disk)
    print(f"launching: {' '.join(command)}", flush=True)
    proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

    screenshots: list[str] = []
    timeline: list[dict] = []
    failure: str | None = None
    serial = SerialLog(serial_log, args.fail_on)
    qmp: Qmp | None = None
    try:
        qmp = Qmp("127.0.0.1", port, args.timeout)
        if resolve_accel(args.accel, qemu) == "none":
            qmp.key_interval = TCG_KEY_INTERVAL
        try:
            screenshots = run_steps(qmp, steps, out_dir, time.time(), serial,
                                    args.wait_timeout, timeline)
        except StepFailed as exc:
            failure = str(exc)
            print(f"session FAILED: {failure}", file=sys.stderr, flush=True)
            if "timed out" in failure:
                capture_hang_state(qmp, out_dir, serial)
            try:
                shot = qmp.screenshot(out_dir / "shot_failed")
                screenshots.append(shot.name)
                print(f"captured {shot}", flush=True)
            except Exception as shot_error:
                print(f"(could not capture failure screenshot: {shot_error})",
                      file=sys.stderr)
            print("--- serial tail ---", file=sys.stderr)
            print(serial.tail(), file=sys.stderr, flush=True)
        try:
            qmp.execute("quit")
        except Exception:
            pass
    finally:
        if qmp is not None:
            qmp.close()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()

    summary = {
        "qemu": qemu,
        "image": image,
        "screenshots": screenshots,
        "serial_log": serial_log.name if serial_log.exists() else None,
        "exit_code": proc.returncode,
        "ok": failure is None,
        "failure": failure,
        "timeline": timeline,
    }
    (out_dir / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary, indent=2))
    return 0 if failure is None else 1


if __name__ == "__main__":
    raise SystemExit(main())
