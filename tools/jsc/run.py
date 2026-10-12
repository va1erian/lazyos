#!/usr/bin/env python3
"""Boot LazyOS with ``jsc``, ``jsc-jit`` and ``cxxproof`` and judge what they print.

Needs ``python tools/jsc/build.py --jit`` first (``target/jsc/bin/``). Then:

1. writes the stress-test bundle (``stress.py``: ``target/jsc/jsc-tests/``; it
   fetches from the network once) unless it exists;
2. builds the ``jsc`` fixture (``tools/abi/fixtures/src/jsc.rs``) for static
   musl, if it is not built;
3. builds an image with the fixture as ``/system/bin/abi-init`` and
   ``LAZYOS_JSC=1`` (the programs in ``/system/bin``, the tests in
   ``/system/share/jsc-tests``) into ``target/jsc/lazyos-jsc.img``;
4. boots it headless with 2 GiB (a static ``jsc`` is tens of MB and LazyOS
   loads an ELF whole into RAM) and waits for ``ABI:jsc:PASS|FAIL``;
5. judges ``shots/jsc/serial.log`` (``judge.py``) and prints each verdict, the
   JIT's speedup over the interpreter and every stress failure.

Usage::

    python tools/jsc/run.py [--qemu PATH] [--accel none] [--stage w1]
    python tools/jsc/run.py --no-build        # boot the last image again
    python tools/jsc/run.py --judge-only LOG  # judge a saved serial log
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "abi"))
import judge  # noqa: E402

BIN = ROOT / "target" / "jsc" / "bin"
TESTS = ROOT / "target" / "jsc" / "jsc-tests"
IMAGE = ROOT / "target" / "jsc" / "lazyos-jsc.img"
OUT = ROOT / "shots" / "jsc"
FIXTURE = ROOT / "target" / "abi" / "fixtures" / "jsc.elf"
PROGRAMS = ("jsc", "jsc-jit", "cxxproof")


def build_fixture() -> bool:
    """Compile just the `jsc` fixture (the whole set is `tools/abi/build.py`)."""
    import build as abi_build  # tools/abi/build.py
    if not abi_build.ensure_target():
        print("jsc: the x86_64-unknown-linux-musl target is unavailable", file=sys.stderr)
        return False
    done = abi_build.run(
        ["cargo", "build", "--manifest-path", str(abi_build.FIXTURES / "Cargo.toml"),
         "--target", abi_build.TARGET, "--release", "--bin", "jsc"],
        env=abi_build.linker_env())
    if done.returncode != 0:
        print(done.stderr[-2000:], file=sys.stderr)
        return False
    built = abi_build.FIXTURES / "target" / abi_build.TARGET / "release" / "jsc"
    FIXTURE.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(built, FIXTURE)
    return True


def build_image(limits: list[str]) -> bool:
    env = dict(os.environ)
    env.update({"LAZYOS_INIT": str(FIXTURE), "LAZYOS_JSC": "1", "LAZYOS_RESET_OS": "1"})
    # The shells and cxxproof are ~130 MB with ICU's data; the volume needs room.
    env.setdefault("LAZYOS_OS_SIZE", "512M")
    for key in ("LAZYOS_LINUXAPPS", "LAZYOS_BUSYBOX", "LAZYOS_BUSYBOX_TEST"):
        env.pop(key, None)
    if limits:
        import run as abi_run  # tools/abi/run.py
        env.update(abi_run.limit_env(limits))
    done = subprocess.run(["cargo", "build"], cwd=ROOT, env=env,
                          capture_output=True, text=True)
    if done.returncode != 0:
        print(done.stderr[-2500:], file=sys.stderr)
        return False
    IMAGE.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(ROOT / "target" / "lazyos.img", IMAGE)
    return True


def boot(args: argparse.Namespace) -> str:
    """Boot the image, wait for the fixture's last line, the serial log."""
    OUT.mkdir(parents=True, exist_ok=True)
    script = OUT / "session.json"
    script.write_text(json.dumps([
        {"wait_for": r"ABI:jsc:(PASS|FAIL)", "regex": True, "timeout": args.timeout},
        {"wait": 1.0},
        {"quit": True},
    ]))
    log = OUT / "serial.log"
    log.unlink(missing_ok=True)
    command = [sys.executable, str(ROOT / "tools" / "screenshot" / "qemu_session.py"),
               "--image", str(IMAGE), "--out", str(OUT), "--script", str(script),
               "--memory", args.memory, "--accel", args.accel,
               "--wait-timeout", str(args.timeout)]
    if args.qemu:
        command += ["--qemu", args.qemu]
    subprocess.run(command, cwd=ROOT)
    return log.read_text(errors="replace") if log.is_file() else ""


def report(serial: str, stages: tuple[str, ...]) -> int:
    verdicts = judge.judge(serial, stages)
    for verdict in verdicts:
        print(f"{'PASS' if verdict.ok else 'FAIL'}  {verdict.name}  {verdict.detail}".rstrip())
    for prefix, program in judge.SHELLS.items():
        ms = judge.startup_ms(serial, prefix)
        if ms is not None:
            print(f"{program} -e 'print(1+1)' took {ms} ms in the guest")
        for name, took in judge.times(serial, prefix).items():
            print(f"{program}: script {name} took {took} ms in the guest")
        took = judge.stress_ms(serial, prefix)
        if took is not None:
            print(f"{program}: the stress subset took {took} ms in the guest")
        for name, reason in judge.stress_failures(serial, prefix).items():
            print(f"{program}: stress FAIL {name}: {reason}")
    plain, jit = judge.times(serial, "JSC:"), judge.times(serial, "JSC:JIT:")
    for name in judge.RUNS:
        if plain.get(name) and jit.get(name):
            print(f"jit speedup on {name}: {plain[name] / jit[name]:.2f}x "
                  f"({plain[name]} ms -> {jit[name]} ms)")
    for name in PROGRAMS:
        if (BIN / name).is_file():
            print(f"{name}: {(BIN / name).stat().st_size / 1048576:.1f} MiB")
    return 0 if judge.passed(verdicts) else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--memory", default="2G", help="guest RAM (default 2G)")
    parser.add_argument("--timeout", type=float, default=3600.0,
                        help="seconds to wait for the fixture to finish")
    parser.add_argument("--limit", action="append", default=[], metavar="KEY=VALUE",
                        help="kernel limit for the image (docs/architecture/limits.md)")
    parser.add_argument("--stage", choices=["w1", "w2"], default="w2",
                        help="w1 judges only the C++/ICU proof (default: w2, both)")
    parser.add_argument("--no-build", action="store_true", help="boot the last image")
    parser.add_argument("--judge-only", metavar="LOG", help="judge a saved serial log and exit")
    args = parser.parse_args()
    stages = ("w1",) if args.stage == "w1" else ("w1", "w2")

    if args.judge_only:
        return report(Path(args.judge_only).read_text(errors="replace"), stages)
    if not args.no_build:
        for name in PROGRAMS if "w2" in stages else ("cxxproof",):
            if not (BIN / name).is_file():
                print(f"jsc: target/jsc/bin/{name} is missing "
                      "(python tools/jsc/build.py --jit)", file=sys.stderr)
                return 1
        if not (TESTS / "index.txt").is_file():
            import stress
            stress.bundle()
        if not build_fixture() or not build_image(args.limit):
            return 1
    return report(boot(args), stages)


if __name__ == "__main__":
    raise SystemExit(main())
