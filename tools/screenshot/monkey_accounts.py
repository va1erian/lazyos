"""Account mode for ``monkey.py --accounts`` (issue #626, docs/accounts-plan.md).

A weighted action profile aimed at the surfaces the accounts work adds: the
login screen and logout, Settings and its Accounts page, the Installer, the
``elevd`` password prompt, and the Terminal typing account-related commands
(``id``, ``su``, ``passwd``, writes to ``/system``, ``/conf``,
``/home/admin``) and random or wrong passwords.

Targets come from the ``UI:RECT`` / ``UI:WIDGET`` lines an image built with
``LAZYOS_UI_PROBE=1`` prints (``session_pointer.py``). A target the guest has
not printed (no login screen or Accounts page before U0-U2 land) is skipped,
never a failure: the action is recorded in ``actions.jsonl`` all the same and
counted under ``accounts.skipped`` in ``report.json``, so a recorded run
replays against an image that has more or fewer of them. Shutdown, restart and
power rows are never targeted (they look like a freeze: ``BAD_TARGET``).

Invariant (forward-looking, harmless today): a grant marker
(``ELEVD:GRANT``, ``ACCT:...GRANT``, ``--grant-pattern``) is a finding unless
a password action marked correct (``--admin-password``) preceded it; the
random passwords the monkey types are all wrong. ``ACCT:ATTACK:...:SUCCEEDED``
is a finding too. Findings reach ``monkey.py`` as ``ACCT-INVARIANT:`` lines.
"""

from __future__ import annotations

import collections
import json
import re
import time

from session_pointer import _CORNER_STEP, _RECT, _WIDGET, POINTER, resolve_pixel

GRANT = r"\b(?:ELEVD|ACCT):[A-Za-z_:]*GRANT"
ATTACK_OK = re.compile(r"ACCT:ATTACK:\S*:SUCCEEDED")
FINDING_PREFIX = "ACCT-INVARIANT:"
BAD_TARGET = re.compile(r"shut|power|restart|reboot|halt|poweroff|suspend", re.I)
INTEREST = re.compile(r"setting|install|account|user|password|login|log ?in|log ?out|sign ?out|elev|"
                      r"admin|passwd|permission|grant|allow|deny|cancel|ok$|lock", re.I)
# What `open` looks for in the start menu and its submenus, most wanted first.
APPS = ["Settings", "Package Installer", "Log out...", "Log out now", "Accounts", "Users"]
FOCUS = ["elevd:password", "login:password", "dialog:password"]

# Share of each kind in the account profile; "base" is the plain monkey's own
# mix (moves, clicks, keys, bursts), kept at a third so windows still move.
PROFILE = {"base": 28, "open": 14, "goto": 10, "shell": 16, "password": 14, "logout": 3, "focus_term": 4}
SHELL = [
    "id", "whoami", "id -u", "groups", "su", "su admin", "su root", "passwd", "passwd admin", "login",
    "sudo id", "pkgctl list", "pkgctl install /system/share/samples/modplayer.lzp", "pkgctl remove x",
    "echo x > /system/pwn", "echo x >> /system/etc/passwd", "touch /conf/pwn", "mkdir /home/admin/pwn",
    "ls -l /home/admin", "ls /conf", "cat /system/etc/passwd", "cat /system/etc/shadow",
    "echo x > /home/admin/pwn", "chmod 777 /system/etc/passwd", "chown 1000 /system/bin/busybox",
    "cp /system/bin/busybox /system/bin/b2", "ls -l /system /conf /home", "ls -ld /system/bin /home/admin",
    "kill -9 1", "kill -9 2", "ps", "echo $USER; echo $HOME", "cd /home/admin; ls", "cd /home; ls",
]
WORDS = ["admin", "root", "password", "hunter2", "letmein", "1234", "qwerty", "toor", "user", "lazyos"]
SYMBOLS = "!@#$%^&*()-_=+[]{};:,./?"
ALNUM = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"


def random_password(rng) -> str:
    """A wrong password: a common word, noise, empty, or absurdly long."""
    kind = rng.choice(["word", "noise", "noise", "empty", "long", "mixed"])
    if kind == "word":
        return rng.choice(WORDS)
    if kind == "empty":
        return ""
    if kind == "long":
        return "".join(rng.choice(ALNUM) for _ in range(rng.randint(100, 300)))
    pool = ALNUM + SYMBOLS if kind == "mixed" else ALNUM
    return "".join(rng.choice(pool) for _ in range(rng.randint(1, 24)))


ACTIONS = ("open", "goto", "acct_shell", "password")


def uses_accounts(replay: list[dict] | None) -> bool:
    """Whether a recorded sequence holds account-mode actions."""
    return any(act["a"] in ACTIONS for act in replay or [])


def move_to(qmp, pixel: tuple[int, int]) -> None:
    """The pointer on ``pixel``, homing to the top-left corner first (relative
    PS/2 motion). Faster than ``session_pointer.move_pointer``: a soak does
    hundreds of these."""
    width, height = POINTER["screen"]
    for _ in range(-(-max(width, height) // _CORNER_STEP) + 1):
        qmp.mouse_move(-_CORNER_STEP, -_CORNER_STEP)
        time.sleep(0.04)
    qmp.mouse_move(*pixel)
    time.sleep(0.08)


def profile_kind(rng, weights: dict | None = None) -> str:
    weights = weights or PROFILE
    return rng.choices(list(weights), list(weights.values()))[0]


class Probe:
    """The newest ``UI:RECT`` / ``UI:WIDGET`` line per target seen on serial."""

    def __init__(self) -> None:
        self.lines: dict[tuple, str] = {}

    def feed(self, line: str) -> None:
        if "UI:" not in line:
            return
        if (m := _RECT.search(line)):
            self.lines[("R", m.group(5).strip())] = m.group(0)
        elif (m := _WIDGET.search(line)):
            self.lines[("W", m.group(6).strip(), m.group(5))] = m.group(0)

    def text(self) -> str:
        return "\n".join(self.lines.values())

    def forget_menus(self) -> None:
        for key in [k for k in self.lines if k[0] == "R" and k[1].startswith("menu:")]:
            del self.lines[key]

    def rect_names(self) -> list[str]:
        return sorted(k[1] for k in self.lines if k[0] == "R")

    def widgets(self) -> list[tuple[str, str]]:
        return sorted((k[1], k[2]) for k in self.lines if k[0] == "W")

    def pixel(self, target) -> tuple[int, int] | None:
        return resolve_pixel(target, {}, self.text())


def goto_candidates(probe: Probe) -> list:
    """Targets worth clicking now: interesting windows, login/elevd/dialog
    rectangles and the widgets of interesting windows; never power rows."""
    out: list = []
    for name in probe.rect_names():
        if name.startswith("menu:") or name.startswith("taskbar:") or BAD_TARGET.search(name):
            continue
        if INTEREST.search(name) or name.split(":")[0] in ("login", "elevd", "dialog"):
            out.append(name)
    for window, widget in probe.widgets():
        if INTEREST.search(window + " " + widget) and not BAD_TARGET.search(window + widget):
            out.append({"window": window, "widget": widget})
    return out


class Invariants:
    """The account invariants over the serial stream."""

    def __init__(self, grant_pattern: str = GRANT) -> None:
        self.grant = re.compile(grant_pattern)
        self.credits = 0
        self.grants = 0

    def credit(self) -> None:
        """An admin password was typed: one grant may now follow."""
        self.credits += 1

    def observe(self, lines: list[str]) -> list[str]:
        found = []
        for line in lines:
            if self.grant.search(line):
                self.grants += 1
                if self.credits > 0:
                    self.credits -= 1
                else:
                    found.append(f"{FINDING_PREFIX} grant without an admin password action: {line.strip()}")
            elif ATTACK_OK.search(line):
                found.append(f"{FINDING_PREFIX} attack succeeded: {line.strip()}")
        return found


class AccountMonkey:
    """Wraps the plain ``Monkey`` (same RNG, same ``perform`` for its actions)."""

    KINDS = ("open", "goto", "shell", "password", "logout", "focus_term")

    def __init__(self, inner, pump, admin_password: str | None = None, grant_pattern: str = GRANT) -> None:
        self.inner, self.qmp, self.rng, self.pump = inner, inner.qmp, inner.rng, pump
        self.probe, self.inv = Probe(), Invariants(grant_pattern)
        self.admin_password = admin_password
        self.done: collections.Counter = collections.Counter()
        self.skipped: collections.Counter = collections.Counter()

    # -- the scan hook ------------------------------------------------------
    def observe(self, lines: list[str]) -> list[str]:
        """Feed serial lines; returns them plus any invariant finding lines."""
        for line in lines:
            self.probe.feed(line)
        return lines + self.inv.observe(lines)

    def summary(self) -> dict:
        return {"done": dict(self.done), "skipped": dict(self.skipped), "grants_seen": self.inv.grants,
                "probe_targets": self.probe.rect_names()[:60]}

    # -- drawing ------------------------------------------------------------
    def next_action(self) -> dict:
        r = self.rng
        kind = profile_kind(r)
        if kind == "open":
            return {"a": "open", "app": r.choice(APPS)}
        if kind == "logout":
            return {"a": "open", "app": r.choice(["Log out...", "Log out now"])}
        if kind == "goto":
            self.pump()
            cands = goto_candidates(self.probe)
            return {"a": "goto", "t": r.choice(cands)} if cands else {"a": "goto", "t": None}
        if kind == "shell":
            return {"a": "acct_shell", "s": r.choice(SHELL) + "\n"}
        if kind == "focus_term":
            return {"a": "goto", "t": {"window": "Terminal"}}
        if kind == "password":
            good = self.admin_password is not None and r.random() < 0.25
            return {"a": "password", "s": self.admin_password if good else random_password(r),
                    "ok": good, "submit": r.random() < 0.9}
        return self.inner.next_action()

    # -- performing ---------------------------------------------------------
    def perform(self, act: dict) -> None:
        kind = act["a"]
        if kind == "open":
            self.done[kind] += 1
            self._open(act["app"])
        elif kind == "goto":
            self._count(kind, act.get("t") and self._click(act["t"]), json.dumps(act.get("t")))
        elif kind == "acct_shell":
            self._count(kind, True)
            self._click({"window": "Terminal"})  # focus it when it is there; typing goes on regardless
            self.qmp.type_text(act["s"])
        elif kind == "password":
            self._count(kind, True)
            for name in FOCUS:
                if self._click(name):
                    break
            if act.get("ok"):
                self.inv.credit()
            self.qmp.type_text(act["s"] + ("\n" if act.get("submit", True) else ""))
        else:
            self.inner.perform(act)

    def _count(self, kind: str, ok, what: str = "") -> None:
        (self.done if ok else self.skipped)[kind if ok else f"{kind}:{what[:40]}"] += 1

    def _click(self, target, button: str = "left") -> bool:
        self.pump()
        pixel = self.probe.pixel(target)
        if pixel is None:
            return False
        move_to(self.qmp, pixel)
        self.qmp.mouse_click(button)
        return True

    def _wait_rect(self, name: str, seconds: float) -> bool:
        deadline = time.time() + seconds
        while True:
            self.pump()
            if self.probe.pixel(name) is not None:
                return True
            if time.time() >= deadline:
                return False
            time.sleep(0.2)

    def _open(self, app: str) -> None:
        """Start menu -> ``app``, trying each category submenu when the row is
        not on the first page. Skipped when the guest never prints the row."""
        self.probe.forget_menus()
        if not self._click("taskbar:start") or not self._wait_rect(f"menu:{app}", 1.5):
            row = f"menu:{app}"
            if self.probe.pixel("taskbar:start") is None:
                self.skipped["open:no taskbar probe"] += 1
                return
            for name in self.probe.rect_names():  # categories, in name order: deterministic
                if name.startswith("menu:") and name != row and not BAD_TARGET.search(name):
                    self._click(name)
                    if self._wait_rect(row, 0.8):
                        break
        if self._click(f"menu:{app}"):
            self.done["open"] += 1
            time.sleep(0.8)
        else:
            self.skipped[f"open:{app}"] += 1
            self.qmp.press_key("esc")  # close what we opened


def add_arguments(p) -> None:
    g = p.add_argument_group("account mode (issue #626)")
    g.add_argument("--accounts", action="store_true",
                   help="weight the profile toward login/logout, Settings, the Installer, elevd and "
                        "account commands in the Terminal; check the account invariants")
    g.add_argument("--accounts-expect-root", action="store_true",
                   help="known-open mode: the desktop still runs as root, so --audit findings are "
                        "reported but do not fail the run")
    g.add_argument("--accounts-user", default="user", help="the session's account; /home/<it> may change")
    g.add_argument("--admin-password", help="the real admin password: a quarter of the password "
                                            "actions use it, and only those may precede a grant marker")
    g.add_argument("--grant-pattern", default=GRANT, metavar="REGEX", help="serial marker of a granted elevation")
    g.add_argument("--audit", action="store_true",
                   help="boot a COPY of the image without -snapshot and diff /system /conf /apps /home "
                        "(tree, owner, mode, content hash) before and after")
    g.add_argument("--audit-root", action="append", default=[], metavar="PATH", help="tree to audit (repeatable)")
    g.add_argument("--audit-allow", action="append", default=[], metavar="PREFIX",
                   help="extra path prefix whose changes are expected (repeatable)")


def attach(args, inner, pump, serial):
    """The account monkey for ``--accounts``, else ``None``. The boot's probe
    lines (the start button is printed once, before the desktop marker the
    monkey waited for) are read back from ``serial``; they are not invariant
    input."""
    if not args.accounts:
        return None
    acct = AccountMonkey(inner, pump, args.admin_password, args.grant_pattern)
    for line in serial.read_text(errors="replace").splitlines():
        acct.probe.feed(line)
    return acct
