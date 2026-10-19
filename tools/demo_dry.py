"""`run_demo.py --dry-run`: show what a set of flags would build, build nothing.

It runs `run_demo.main` for real with every prerequisite build and `cargo
build` replaced by a recorder, so the flags, the environment and the steps are
the ones the run would use, not a second copy of its logic. Nothing is built,
erased or booted (`--build-only` is added; a question about recreating the OS
volume is answered yes, since nothing is erased).
"""

from __future__ import annotations

import os
import subprocess
from types import SimpleNamespace
from unittest import mock

import suites


def _flags_on(args) -> list[str]:
    """The boolean `--flags` that are on after the suite and implications."""
    return [f"--{name.replace('_', '-')}" for name, value in sorted(vars(args).items())
            if value is True and name not in ("dry_run", "build_only", "yes")]


def run(module, argv: list[str], args) -> int:
    """Run `module.main` (`run_demo`) on `argv` with the builds recorded and print the report."""
    steps: list[str] = []
    cargo: list[tuple[list[str], dict]] = []

    def recorder(name: str):
        def step(*_args, **_kwargs):
            steps.append(name)
            return True
        return step

    def fake_run(command, *_args, env=None, **_kwargs):
        if command and command[0] == "cargo":
            cargo.append((list(command), dict(env or os.environ)))
        else:
            steps.append(" ".join(str(part) for part in command))
        return subprocess.CompletedProcess(command, 0)

    patches = [mock.patch.object(module, name, recorder(name[len("build_"):]))
               for name, value in vars(module).items()
               if name.startswith("build_") and callable(value)]
    patches += [mock.patch.object(module, "require_busybox", lambda _allow: True),
                mock.patch.object(module, "confirm", lambda _question: True),
                mock.patch.object(module.subprocess, "run", fake_run)]
    inner = [a for a in argv if a != "--dry-run"] + ["--build-only"]
    for patch in patches:
        patch.start()
    try:
        code = module.main(inner)
    finally:
        mock.patch.stopall()
    _report(args, steps, cargo)
    return code


def _report(args, steps: list[str], cargo: list[tuple[list[str], dict]]) -> None:
    if args.suite:
        print(suites.describe(args.suite, args.with_options))
    print("flags:", " ".join(_flags_on(args)) or "(none)")
    print("build steps, in order:")
    for number, step in enumerate(steps, 1):
        print(f"  {number}. {step}")
    for command, env in cargo:
        print(f"  {len(steps) + 1}. {' '.join(command)}")
        print("environment:")
        for key in sorted(k for k in env if k.startswith(("LAZYOS_", "LAZYRAD_"))
                          and env[k] != os.environ.get(k)):
            print(f"  {key}={env[key]}")
    if not cargo:
        print("(the image build was not reached)")
