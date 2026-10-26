#!/usr/bin/env python3
"""Run LazyVM command lines inside a real LazyOS boot and write what each printed.

LazyVM's test tool stages its VM, class library and tests (``LAZYOS_LAZYVM_STAGE``, default
``target/lazyvm/stage``; docs/agents/lazyvm.md) and calls::

    python tools/lazyvm/run.py --commands FILE --out DIR [--image IMG] [--no-build]
                               [--accel auto|none|tcg|whpx|kvm] [--timeout SECONDS]

``FILE`` holds one command per line, with LazyOS paths (``/system/lazyvm/bin/lazyvm -cp
/system/lazyvm/tests s1.Hello``); words split as a POSIX shell would (``shlex``), blank lines
and ``#`` comments are skipped. The tool

1. writes the hex command list the ``lazyvm`` ABI fixture reads (``DIR/commands.list``);
2. unless ``--no-build``, builds that fixture and an image with it as ``/system/bin/abi-init``,
   ``LAZYOS_LAZYVM=1`` and the list (``LAZYOS_LAZYVM_COMMANDS``), copied to ``IMG``;
3. boots ``IMG`` headless; the fixture runs each command in order, stdin closed, and sends
   its output hex-encoded over the serial log (``tools/lazyvm/judge.py``);
4. writes ``DIR/N.stdout``, ``DIR/N.stderr`` (separate, byte exact) and ``DIR/N.exit`` (the
   status in decimal: 128 + the signal for a killed program, 124 after ``--timeout``, 127 when
   it could not start) for command ``N`` (from 1).

It exits 0 when every command ran, whatever their own statuses, and 1 with a message when the
build, the boot or the harness failed. ``--no-build`` boots ``IMG`` as it is: it must have
been built for the same command list (the tool checks and says so).
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import judge  # noqa: E402

IMAGE = ROOT / "target" / "lazyvm" / "lazyos-lazyvm.img"
FIXTURE = ROOT / "target" / "abi" / "fixtures" / "lazyvm.elf"
#: Seconds the boot may take to reach the fixture, and the slack past a command's own timeout.
BOOT_SECONDS = 300
SLACK_SECONDS = 60


def read_commands(path: Path) -> list[list[str]]:
    """The argument vectors of the command file."""
    commands = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip() and not line.lstrip().startswith("#"):
            commands.append(shlex.split(line))
    return commands


def build_fixture() -> bool:
    """Compile the `lazyvm` fixture (the bench's whole set is `tools/abi/build.py`)."""
    sys.path.insert(0, str(ROOT / "tools" / "abi"))
    import build as abi_build  # tools/abi/build.py
    if not abi_build.ensure_target():
        return fail("the x86_64-unknown-linux-musl target is unavailable")
    done = abi_build.run(
        ["cargo", "build", "--manifest-path", str(abi_build.FIXTURES / "Cargo.toml"),
         "--target", abi_build.TARGET, "--release", "--bin", "lazyvm"],
        env=abi_build.linker_env())
    if done.returncode != 0:
        return fail(f"the lazyvm fixture did not build:\n{done.stderr[-2000:]}")
    FIXTURE.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(abi_build.FIXTURES / "target" / abi_build.TARGET / "release" / "lazyvm",
                    FIXTURE)
    return True


def build_image(listing: Path, image: Path) -> bool:
    """Build an image whose boot is the fixture over `listing`, and copy it to `image`."""
    env = dict(os.environ)
    # A fresh, small volume per run, as the ABI bench builds; never a developer's image updated
    # in place, and no other fixture or program shadowing `abi-init`.
    env.setdefault("LAZYOS_OS_SIZE", "256M")
    env["LAZYOS_RESET_OS"] = "1"
    for key in ("LAZYOS_BUSYBOX_TEST", "LAZYOS_LINUXAPPS", "LAZYOS_DYNLINK"):
        env.pop(key, None)
    env.update({"LAZYOS_INIT": str(FIXTURE), "LAZYOS_LAZYVM": "1",
                "LAZYOS_LAZYVM_COMMANDS": str(listing)})
    done = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if done.returncode != 0:
        return fail(f"the image build failed:\n{done.stderr[-2500:]}")
    image.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(ROOT / "target" / "lazyos.img", image)
    return True


def session(count: int, timeout: int) -> list[dict]:
    """The QEMU session: wait for the fixture, each command's end, then the last line."""
    steps = [{"wait_for": "LAZYVM:INIT:", "timeout": BOOT_SECONDS}]
    steps += [{"wait_for": f"LAZYVM:END:{n}:", "timeout": timeout + SLACK_SECONDS}
              for n in range(1, count + 1)]
    return steps + [{"wait_for": "LAZYVM:DONE", "timeout": SLACK_SECONDS}, {"quit": True}]


def boot(args: argparse.Namespace, count: int, qemu_dir: Path) -> str:
    """Boot the image through `qemu_session.py`; the serial log."""
    qemu_dir.mkdir(parents=True, exist_ok=True)
    script = qemu_dir / "session.json"
    script.write_text(json.dumps(session(count, args.timeout)))
    log = qemu_dir / "serial.log"
    log.unlink(missing_ok=True)
    command = [sys.executable, str(ROOT / "tools" / "screenshot" / "qemu_session.py"),
               "--image", str(args.image), "--out", str(qemu_dir), "--script", str(script),
               "--memory", args.memory, "--accel", args.accel,
               "--fail-on", r"LAZYVM:FAIL:", "--fail-on", r"ABI:INIT:FAIL"]
    if args.qemu:
        command += ["--qemu", args.qemu]
    done = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    if done.returncode != 0:
        print(done.stdout[-3000:], done.stderr[-2000:], sep="\n", file=sys.stderr)
    return log.read_text(encoding="utf-8", errors="replace") if log.is_file() else ""


def write_results(run: judge.Run, lines: list[str], out: Path) -> list[str]:
    """Write each command's files; the problems that make the run a failure."""
    problems = list(run.errors)
    if run.failure:
        problems.append(f"the fixture failed: {run.failure}")
    if run.count is not None and run.count != len(lines):
        problems.append(f"the image holds {run.count} commands, not {len(lines)}")
    for n, line in enumerate(lines, start=1):
        result = run.results.get(n)
        if result is None or result.exit is None:
            problems.append(f"command {n} did not finish: {line}")
            continue
        if result.line != line.encode():
            problems.append(f"command {n} in the image is another one (rebuild without "
                            "--no-build)")
            continue
        if result.spawn_error:
            print(f"lazyvm: command {n} did not start: {result.spawn_error}", file=sys.stderr)
        if result.timed_out:
            print(f"lazyvm: command {n} was killed after its timeout", file=sys.stderr)
        (out / f"{n}.stdout").write_bytes(result.stdout)
        (out / f"{n}.stderr").write_bytes(result.stderr)
        (out / f"{n}.exit").write_bytes(f"{result.exit}\n".encode())
    if not run.done and not problems:
        problems.append("the fixture never printed LAZYVM:DONE")
    return problems


def fail(message: str) -> bool:
    print(f"lazyvm: {message}", file=sys.stderr)
    return False


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--commands", type=Path, required=True, help="one command per line")
    parser.add_argument("--out", type=Path, required=True, help="where N.stdout/stderr/exit go")
    parser.add_argument("--image", type=Path, default=IMAGE, help=f"default {IMAGE}")
    parser.add_argument("--no-build", action="store_true", help="boot --image as it is")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--timeout", type=int, default=120, help="seconds per command")
    parser.add_argument("--memory", default="1G", help="guest RAM (default 1G)")
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    args = parser.parse_args()

    commands = read_commands(args.commands)
    if not commands:
        return int(not fail(f"{args.commands} holds no command"))
    args.out.mkdir(parents=True, exist_ok=True)
    lines = [judge.encode(args.timeout, argv) for argv in commands]
    listing = args.out / "commands.list"
    listing.write_text("".join(f"{line}\n" for line in lines), encoding="ascii")
    if not args.no_build and not (build_fixture() and build_image(listing.resolve(), args.image)):
        return 1
    if not args.image.is_file():
        return int(not fail(f"no image at {args.image} (run without --no-build)"))
    serial = boot(args, len(lines), args.out / "qemu")
    problems = write_results(judge.parse(serial), lines, args.out)
    for problem in problems:
        fail(problem)
    if problems:
        fail(f"serial log: {args.out / 'qemu' / 'serial.log'}")
        return 1
    print(f"lazyvm: {len(lines)} commands ran; results in {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
