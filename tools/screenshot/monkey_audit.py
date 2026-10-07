"""Host-side audit of the OS volume for ``monkey.py --audit`` (issue #626).

The plain monkey boots with ``-snapshot`` so the guest can never damage the
image. ``--audit`` boots a temporary *copy* of it instead, without
``-snapshot``, so what the session wrote lands on the copy's ext2 OS volume,
and compares the tree before and after with ``osread tree`` (shared with
``tools/accounts/audit.py``; ``libs/ext2fs/examples/osread.rs``) plus ``osread
stat`` for each root itself: every node's kind, mode, owner, size and a
content hash (mtimes are ignored). The "before" is taken after a first, input-free boot of the
same copy, so what the boot itself writes (``pkgd`` installing the core
packages into ``/apps``, ``confd`` settling) is not blamed on the input.

A change is a finding unless it is under ``/home/<user>``, ``/transient``,
``/tmp`` or ``/logs`` (``DEFAULT_ALLOWED``). The session has run as ``user``
since U0 (#623), so a finding fails the run; ``--accounts-expect-root`` (the
known-open mode kept from when the desktop ran as root) reports them without
failing it.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOTS = ["/system", "/conf", "/apps", "/home"]
DEFAULT_ALLOWED = ["/home/{user}", "/transient", "/tmp", "/logs"]
FLUSH_WAIT = 7.0  # the block cache commits every 5 s (AGENTS.md, ext2 library)
FIELDS = ("kind", "mode", "uid", "gid", "size", "hash")


def parse_tree(text: str) -> dict[str, tuple]:
    """``osread tree`` output (``<d|f> mode uid gid size mtime hash path``) as
    ``{path: (kind, mode, uid, gid, size, hash)}``. The mtime is dropped (a
    read can touch it), and so is a directory's size, which only follows its
    entries: an added or removed entry is reported on its own path."""
    nodes: dict[str, tuple] = {}
    for line in text.splitlines():
        parts = line.rstrip("\r").split(" ", 7)
        if len(parts) == 8 and parts[0] in ("d", "f"):
            kind, mode, uid, gid, size, _mtime, hash_, path = parts
            nodes[path] = (kind, mode, uid, gid, "0" if kind == "d" else size, hash_)
    return nodes


def parse_root_stat(root: str, text: str) -> dict[str, tuple]:
    """``osread stat ROOT`` (``mode uid gid size``) as a ``parse_tree`` node, so
    a chmod or chown of the root itself is caught too."""
    parts = text.split()
    if len(parts) != 4:
        return {}
    mode, uid, gid, _size = parts
    return {root: ("d", mode, uid, gid, "0", "-")}


def diff_trees(before: dict[str, tuple], after: dict[str, tuple]) -> list[dict]:
    """Every added, removed or modified path, sorted by path."""
    changes = []
    for path in sorted(before.keys() | after.keys()):
        old, new = before.get(path), after.get(path)
        if old == new:
            continue
        if old is None:
            changes.append({"path": path, "change": "added", "new": new})
        elif new is None:
            changes.append({"path": path, "change": "removed", "old": old})
        else:
            fields = [name for name, a, b in zip(FIELDS, old, new) if a != b]
            changes.append({"path": path, "change": "modified:" + ",".join(fields), "old": old, "new": new})
    return changes


def allowed_prefixes(user: str, extra: list[str] | None = None) -> list[str]:
    return [p.format(user=user) for p in DEFAULT_ALLOWED] + list(extra or [])


def is_allowed(path: str, prefixes: list[str]) -> bool:
    return any(path == p or path.startswith(p.rstrip("/") + "/") for p in prefixes)


def findings_of(changes: list[dict], prefixes: list[str]) -> list[str]:
    """One line per change outside the allowed trees. A directory whose only
    change is its size (an entry came or went) is not reported twice: the
    entry itself is."""
    out = []
    for change in changes:
        if is_allowed(change["path"], prefixes):
            continue
        if change["change"] == "modified:size" and change["old"][0] == "d":
            continue
        out.append(f"AUDIT: {change['change']} {change['path']}")
    return out


def osread_binary(root: Path) -> list[str]:
    """Build ``osread`` once and return its path; far cheaper than a
    ``cargo run`` per query."""
    subprocess.run(["cargo", "build", "-q", "-p", "ext2fs", "--example", "osread"], cwd=root, check=True)
    target = Path(os.environ.get("CARGO_TARGET_DIR", root / "target"))
    exe = target / "debug" / "examples" / ("osread.exe" if sys.platform == "win32" else "osread")
    if not exe.exists():
        raise FileNotFoundError(exe)
    return [str(exe)]


def snapshot(osread: list[str], image: Path, roots: list[str]) -> dict[str, tuple]:
    nodes: dict[str, tuple] = {}
    for root in roots:
        stat = subprocess.run([*osread, str(image), "stat", root], capture_output=True, text=True)
        if stat.returncode == 0:
            nodes.update(parse_root_stat(root, stat.stdout))
        done = subprocess.run([*osread, str(image), "tree", root], capture_output=True, text=True)
        if done.returncode == 0:
            nodes.update(parse_tree(done.stdout))
    return nodes


def settle_boot(boot) -> bool:
    """Boot the copy once with no input, wait for the desktop and the flusher,
    and stop it. ``boot`` returns ``(proc, qmp, ready)``."""
    proc, qmp, ready = boot()
    try:
        if ready:
            time.sleep(FLUSH_WAIT)
        return ready
    finally:
        try:
            qmp.execute("quit")
        except Exception:
            pass
        qmp.close()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()


def run_audited(args, qemu: str, seed: int, out: Path, replay, run_one, helpers) -> bool:
    """One seed under ``--audit``: copy, baseline boot, monkey run, diff.
    ``helpers`` is ``(start_guest, wait_ready, Tail)`` from ``monkey.py``."""
    start_guest, wait_ready, tail_cls = helpers
    root = Path(__file__).resolve().parents[2]
    out.mkdir(parents=True, exist_ok=True)
    copy = out / "audit_os.img"
    shutil.copyfile(args.image, copy)
    run_args = argparse.Namespace(**{**vars(args), "image": str(copy)})
    osread = osread_binary(root)
    (out / "baseline").mkdir(exist_ok=True)

    def boot():
        proc, qmp, serial = start_guest(run_args, qemu, out / "baseline")
        return proc, qmp, wait_ready(tail_cls(serial), proc, args.marker, args.boot_timeout)

    if not settle_boot(boot):
        print(f"MONKEY: FAULT seed={seed} baseline boot never reached {args.marker}")
        return False
    roots = args.audit_root or ROOTS
    before = snapshot(osread, copy, roots)
    ok = run_one(run_args, qemu, seed, out, replay)
    after = snapshot(osread, copy, roots)
    changes = diff_trees(before, after)
    findings = findings_of(changes, allowed_prefixes(args.accounts_user, args.audit_allow))
    (out / "audit.json").write_text(json.dumps(
        {"roots": roots, "before": len(before), "after": len(after), "changes": changes,
         "findings": findings}, indent=2), encoding="utf-8")
    (out / "audit_diff.txt").write_text("\n".join(findings) + "\n", encoding="utf-8")
    verdict = "known-open (--accounts-expect-root)" if args.accounts_expect_root else "FAULT"
    print(f"MONKEY: AUDIT seed={seed} nodes={len(after)} changes={len(changes)} "
          f"outside-allowed={len(findings)} {verdict if findings else 'clean'}")
    for line in findings[:20]:
        print("MONKEY:   " + line)
    copy.unlink(missing_ok=True)  # the findings are in audit.json; the copy is large
    return ok and (not findings or args.accounts_expect_root)
