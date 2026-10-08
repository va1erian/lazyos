#!/usr/bin/env python3
"""What `tools/smb/fuse_run.py` does in the guest and what it expects back
(docs/smb-plan.md F3): the share mounted with `smbfuse`, then used through
`/mnt/share` by ordinary BusyBox commands, judged from the servers'
directories, the markers and the wire.

Every step echoes `F3:<name>:<status>` so the judge can split the log. The
passwords are typed at `smbfuse`'s prompt (`type_secret`), never in the
script.
"""

from __future__ import annotations

import hashlib
import re
from dataclasses import dataclass
from pathlib import Path

import checks

PASSWORD_VAR = checks.PASSWORD_VAR
WRONG_VAR = checks.WRONG_VAR
HELLO, BIG, SEQ = checks.HELLO, checks.BIG, checks.SEQ
#: What the guest's `seq 1 100000` writes (about 576 KiB: nine 64 KiB
#: requests and a short one).
BIG_TXT = "".join(f"{n}\n" for n in range(1, 100001)).encode()
#: `patch.bin`: SEQ with three bytes written in place at offset 10.
PATCHED = SEQ[:10] + b"XYZ" + SEQ[13:]


@dataclass
class Step:
    name: str
    command: str
    #: Whether the step must exit 0 (else it must fail).
    passes: bool = True
    #: Text the step's output must contain.
    shows: str = ""


SERVERS = [checks.Server("main"), checks.Server("signed", {"require_signing": True})]

MNT = "/mnt/share"
STEPS = [
    Step("ls", f"ls {MNT}", shows="emptydir"),
    Step("cat", f"cat {MNT}/hello.txt", shows=HELLO.decode().strip()),
    Step("md5", f"md5sum {MNT}/big.bin", shows=hashlib.md5(BIG).hexdigest()),
    Step("cpout", f"cp {MNT}/big.bin /tmp/big.bin && md5sum /tmp/big.bin", shows=hashlib.md5(BIG).hexdigest()),
    Step("cpin", f"cp /tmp/big.txt {MNT}/big.txt && cmp /tmp/big.txt {MNT}/big.txt"),
    Step("size", f"wc -c {MNT}/big.txt", shows=str(len(BIG_TXT))),
    Step("patch", f"cp /tmp/seq.txt {MNT}/patch.bin && printf XYZ | dd of={MNT}/patch.bin bs=1 seek=10 "
                  f"conv=notrunc && head -c 16 {MNT}/patch.bin", shows="XYZ"),
    Step("append", f"echo appended >> {MNT}/hello.txt && tail -n 1 {MNT}/hello.txt", shows="appended"),
    Step("mkdir", f"mkdir {MNT}/newdir && cp /tmp/seq.txt {MNT}/newdir/seq.txt && "
                  f"mv {MNT}/newdir/seq.txt {MNT}/newdir/moved.txt && ls {MNT}/newdir", shows="moved.txt"),
    Step("rm", f"rm {MNT}/old.txt && rmdir {MNT}/emptydir && ls {MNT}"),
    Step("notempty", f"rmdir {MNT}/docs", passes=False),
    Step("missing", f"cat {MNT}/nope.txt", passes=False),
    Step("readme", f"cat {MNT}/docs/readme.md", shows="# read me"),
    Step("df", f"df {MNT}"),
    # `sync` reaches every mount's flush: SMB FLUSH of the files written.
    Step("sync", "sync"),
]


def mount(name: str, port: int, user: str, flags: str = "", share: str = "share",
          secret: str = PASSWORD_VAR) -> list[dict]:
    """Mount `//10.0.2.2/<share>` at `/mnt/<name>`: the command prompts, the
    password is typed, and it returns once the daemon has mounted."""
    flags = f"{flags} " if flags else ""
    line = f"smbfuse {flags}-p {port} -U {user} //10.0.2.2/{share} name={name}; echo F3:mount-{name}:$?"
    return [{"type": line},
            {"key": "enter", "until": "Password for", "timeout": 120, "retries": 1},
            {"type_secret": secret},
            {"key": "enter", "until": f"F3:mount-{name}:", "timeout": 180}]


def run_step(name: str, command: str) -> list[dict]:
    return [{"type": f"{command}; echo F3:{name}:$?"},
            {"key": "enter", "until": f"F3:{name}:", "timeout": 180}]


def session(ports: dict[str, int], user: str) -> list[dict]:
    steps: list[dict] = [{"wait_for": "/ #", "timeout": 300}]
    steps += run_step("ready", "seq 1 20000 > /tmp/seq.txt; seq 1 100000 > /tmp/big.txt")
    steps += mount("share", ports["main"], user)
    for step in STEPS:
        steps += run_step(step.name, step.command)
    steps += mount("signed", ports["signed"], user, flags="--sign-required")
    steps += run_step("signedcp", "cp /tmp/seq.txt /mnt/signed/seq.txt && md5sum /mnt/signed/big.bin")
    steps += mount("bad", ports["main"], user, secret=WRONG_VAR)
    steps += mount("noshare", ports["main"], user, share="nope")
    steps += run_step("mounts", "ls /mnt")
    steps += [{"shot": "smbfuse"}, {"quit": True}]
    return steps


#: Mount outcomes: name -> (exit status, a fragment of the FAIL line).
MOUNTS = {"share": (0, ""), "signed": (0, ""), "bad": (5, "LOGON_FAILURE"), "noshare": (8, "BAD_NETWORK_NAME")}


def segments(text: str) -> dict[str, tuple[str, int]]:
    """The serial log split at each `F3:<name>:<status>` marker: what each
    step printed, and its status."""
    out, start = {}, 0
    for match in re.finditer(r"^F3:([a-z0-9-]+):(\d+)", text, re.M):
        out[match.group(1)] = (text[start:match.start()], int(match.group(2)))
        start = match.end()
    return out


def judge_steps(text: str) -> list[str]:
    problems = []
    parts = segments(text)
    for name, (status, reason) in MOUNTS.items():
        got = parts.get(f"mount-{name}")
        if got is None:
            problems.append(f"mount {name}: never finished")
            continue
        output, code = got
        print(f"  mount {name:8} status {code}")
        if code != status:
            problems.append(f"mount {name}: status {code}, expected {status}")
        if status == 0 and not re.search(rf"^SMBFUSE:UP /mnt/{name} dialect=0x0210", output, re.M):
            problems.append(f"mount {name}: no SMBFUSE:UP line")
        if reason and not re.search(rf"^SMBFUSE:FAIL .*{reason}", output, re.M):
            problems.append(f"mount {name}: refused without {reason!r}")
    if "signing=on" not in parts.get("mount-signed", ("", 0))[0]:
        problems.append("mount signed: the logon was not signed")
    for step in STEPS + [Step("signedcp", "", shows=hashlib.md5(BIG).hexdigest())]:
        got = parts.get(step.name)
        if got is None:
            problems.append(f"{step.name}: never finished")
            continue
        output, code = got
        ok = (code == 0) == step.passes and step.shows in output
        print(f"  {step.name:9} status {code} {'ok' if ok else 'WRONG'}")
        if (code == 0) != step.passes:
            problems.append(f"{step.name}: status {code}, expected {'0' if step.passes else 'a failure'}")
        if step.shows not in output:
            problems.append(f"{step.name}: the output lacks {step.shows!r}")
    listing = parts.get("mounts", ("", 1))[0]
    for name in ("bad", "noshare"):
        if re.search(rf"\b{name}\b", listing.split("ls /mnt", 1)[-1]):
            problems.append(f"/mnt lists {name}, whose mount failed")
    return problems


def judge_tree(root: Path, expect: dict[str, bytes], dirs: set[str]) -> list[str]:
    """A server's directory is exactly `expect` (files) and `dirs`."""
    problems = []
    for name, want in expect.items():
        path = root / name
        got = path.read_bytes() if path.is_file() else None
        print(f"  server {root.name}/{name:16} {'ok' if got == want else 'DIFFERS'}")
        if got != want:
            size = len(got) if got is not None else "missing"
            problems.append(f"{root.name}/{name}: {size} bytes on the server, wanted {len(want)}")
    tree = sorted(p.relative_to(root).as_posix() for p in root.rglob("*"))
    want_tree = sorted(set(expect) | dirs)
    if tree != want_tree:
        problems.append(f"{root.name} tree {tree}, expected {want_tree}")
    return problems


MAIN_FILES = {"hello.txt": HELLO + b"appended\n", "big.bin": BIG, "big.txt": BIG_TXT, "patch.bin": PATCHED,
              "newdir/moved.txt": SEQ, "docs/readme.md": b"# read me\n"}
MAIN_DIRS = {"docs", "newdir"}
SIGNED_FILES = {"hello.txt": HELLO, "big.bin": BIG, "old.txt": b"remove me\n", "seq.txt": SEQ,
                "docs/readme.md": b"# read me\n"}
SIGNED_DIRS = {"docs", "emptydir"}
