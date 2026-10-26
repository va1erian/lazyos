#!/usr/bin/env python3
"""Run LazyVM command lines inside a real LazyOS boot and write what each printed.

LazyVM's test tool stages its VM, class library and tests (``LAZYOS_LAZYVM_STAGE``, default
``target/lazyvm/stage``; docs/agents/lazyvm.md) and calls::

    python tools/lazyvm/run.py --commands FILE --out DIR [--image IMG] [--no-build]
                               [--accel auto|none|tcg|whpx|kvm] [--timeout SECONDS]

``FILE`` holds one command per line, with LazyOS paths (``/system/lazyvm/bin/lazyvm -cp
/system/lazyvm/tests s1.Hello``); words split as a POSIX shell would (``shlex``), blank lines
and ``#`` comments are skipped. The tool

1. removes the ``N.stdout``, ``N.stderr`` and ``N.exit`` files a previous run left in ``DIR``
   and writes the hex command list the ``lazyvm`` ABI fixture reads (``DIR/commands.list``);
2. unless ``--no-build``, builds that fixture and an image with it as ``/system/bin/abi-init``,
   ``LAZYOS_LAZYVM=1`` and the list (``LAZYOS_LAZYVM_COMMANDS``) on a fresh OS volume, and
   copies it to ``IMG`` (never ``target/lazyos.img``). The image build always writes
   ``target/lazyos.img``; the tool moves a developer's image there to
   ``target/lazyvm/lazyos.img.saved`` first and puts it back afterwards. If a run could not
   put it back, it says where it is, and the next run refuses to start until it is moved;
3. boots ``IMG`` headless (``boot.py``); the fixture runs each command in order, stdin closed,
   and streams its output hex-encoded over the serial log (``judge.py``);
4. writes ``DIR/N.stdout``, ``DIR/N.stderr`` (separate, byte exact, each at most 8 MiB) and
   ``DIR/N.exit`` (the status in decimal: 128 + the signal for a killed program, 124 after
   ``--timeout``, 125 when no exit status came back, 127 when it could not start) for command
   ``N`` (from 1).

It exits 0 when every command ran, whatever their own statuses, and 1 with a message when the
build, the boot or the harness failed. ``--no-build`` boots ``IMG`` as it is: it must have
been built for the same command list (the tool checks and says so).
"""

from __future__ import annotations

import argparse
import os
import re
import shlex
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import boot  # noqa: E402
import judge  # noqa: E402

IMAGE = ROOT / "target" / "lazyvm" / "lazyos-lazyvm.img"
FIXTURE = ROOT / "target" / "abi" / "fixtures" / "lazyvm.elf"
#: What the image build writes, whatever it is asked for.
BUILT = ROOT / "target" / "lazyos.img"
#: A developer's image, kept here while the tool's own build has `BUILT`.
SAVED = ROOT / "target" / "lazyvm" / "lazyos.img.saved"
#: The caller's `LAZYOS_*` variables the build keeps: the stage, the volume size, the limits.
KEPT = re.compile(r"LAZYOS_(LAZYVM_STAGE|OS_SIZE|LIMIT_\w+)$")
RESULT = re.compile(r"\d+\.(stdout|stderr|exit)$")


def read_commands(path: Path) -> list[list[str]]:
    """The argument vectors of the command file; `ValueError` names a line that does not split."""
    commands = []
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        try:
            commands.append(shlex.split(line))
        except ValueError as error:
            raise ValueError(f"{path}:{number}: {error}: {line}") from None
    return commands


def build_env(environ: dict[str, str], listing: Path) -> dict[str, str]:
    """The image build's environment: the caller's, without the `LAZYOS_*` switches that
    would add to or change this boot, plus the fixture, LazyVM and a fresh small volume."""
    env = {key: value for key, value in environ.items()
           if not key.startswith("LAZYOS_") or KEPT.match(key)}
    env.setdefault("LAZYOS_OS_SIZE", "256M")
    env.update({"LAZYOS_RESET_OS": "1", "LAZYOS_INIT": str(FIXTURE), "LAZYOS_LAZYVM": "1",
                "LAZYOS_LAZYVM_COMMANDS": str(listing)})
    return env


def clear_results(out: Path) -> None:
    """Remove the result files of an earlier run, so none is mistaken for this one's."""
    for path in out.iterdir():
        if path.is_file() and RESULT.fullmatch(path.name):
            path.unlink()


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


def set_aside(built: Path, saved: Path) -> str | None:
    """Move a developer's image at `built` to `saved`; why not, if it must not be done. An
    image already at `saved` is one an earlier run did not put back: never overwrite it."""
    if saved.exists():
        return (f"{saved} holds an image an earlier run did not put back; move it back to "
                f"{built} (or delete it if it is not wanted) and run again")
    if built.is_file():
        saved.parent.mkdir(parents=True, exist_ok=True)
        os.replace(built, saved)
    return None


def put_back(built: Path, saved: Path) -> str | None:
    """Return the image `set_aside` moved to `saved` to `built`; where it was left, if not."""
    if not saved.exists():
        return None
    try:
        os.replace(saved, built)
    except OSError as error:
        return f"could not put the developer image back ({error}); it is at {saved}"
    return None


def build_image(listing: Path, image: Path) -> bool:
    """Build an image whose boot is the fixture over `listing` and copy it to `image`. The
    build writes `BUILT`; a developer's image there is set aside and restored, and a run
    that could not restore it fails, saying where it is."""
    refused = set_aside(BUILT, SAVED)
    if refused:
        return fail(refused)
    try:
        built = run_build(listing, image)
    finally:
        left = put_back(BUILT, SAVED)
    if left:
        return fail(left)
    return built


def run_build(listing: Path, image: Path) -> bool:
    """`cargo build` with the fixture and `listing`, then copy the result to `image`."""
    env = build_env(dict(os.environ), listing)
    done = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if done.returncode != 0:
        return fail(f"the image build failed:\n{done.stderr[-2500:]}")
    image.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(BUILT, image)
    return True


def image_refused(image: Path) -> str | None:
    """Why `--image` cannot be built to: the build's own output, or where it keeps a
    developer's image."""
    same = lambda path: os.path.normcase(str(path.resolve()))  # noqa: E731
    for reserved in (BUILT, SAVED):
        if same(image) == same(reserved):
            return (f"--image {image} is where the image build itself writes or keeps a "
                    f"developer's image; choose another path (default {IMAGE})")
    return None


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
        for note, shown in ((f"did not start: {result.spawn_error}", result.spawn_error),
                            ("was killed after its timeout", result.timed_out),
                            ("printed more than 8 MiB; the rest was dropped", result.truncated),
                            ("kept a pipe open past the wait; output may be missing",
                             result.incomplete)):
            if shown:
                print(f"lazyvm: command {n} {note}", file=sys.stderr)
        (out / f"{n}.stdout").write_bytes(result.stdout)
        (out / f"{n}.stderr").write_bytes(result.stderr)
        (out / f"{n}.exit").write_bytes(f"{result.exit}\n".encode())
    if not run.done and not problems:
        problems.append("the fixture never printed LAZYVM:DONE")
    return problems


def fail(message: str) -> bool:
    print(f"lazyvm: {message}", file=sys.stderr)
    return False


def positive(text: str) -> int:
    value = int(text)
    if value < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return value


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--commands", type=Path, required=True, help="one command per line")
    parser.add_argument("--out", type=Path, required=True, help="where N.stdout/stderr/exit go")
    parser.add_argument("--image", type=Path, default=IMAGE, help=f"default {IMAGE}")
    parser.add_argument("--no-build", action="store_true", help="boot --image as it is")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--timeout", type=positive, default=120, help="seconds per command (>= 1)")
    parser.add_argument("--memory", default="1G", help="guest RAM (default 1G)")
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    args = parser.parse_args()

    try:
        commands = read_commands(args.commands)
    except (OSError, ValueError) as error:
        return int(not fail(str(error)))
    if not commands:
        return int(not fail(f"{args.commands} holds no command"))
    args.out.mkdir(parents=True, exist_ok=True)
    clear_results(args.out)
    lines = [judge.encode(args.timeout, argv) for argv in commands]
    listing = args.out / "commands.list"
    listing.write_text("".join(f"{line}\n" for line in lines), encoding="ascii")
    if not args.no_build:
        refused = image_refused(args.image)
        if refused:
            return int(not fail(refused))
        if not (build_fixture() and build_image(listing.resolve(), args.image)):
            return 1
    if not args.image.is_file():
        return int(not fail(f"no image at {args.image} (run without --no-build)"))
    qemu_dir = args.out / "qemu"
    serial, failure = boot.boot(args.image, args.timeout, qemu_dir, args.accel, args.memory,
                                args.qemu)
    problems = ([failure] if failure else []) + write_results(judge.parse(serial), lines, args.out)
    for problem in problems:
        fail(problem)
    if problems:
        fail(f"serial log: {qemu_dir / 'serial.log'}")
        return 1
    print(f"lazyvm: {len(lines)} commands ran; results in {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
