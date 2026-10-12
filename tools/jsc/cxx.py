"""`cxxproof`: the W1 program, built as a static-PIE musl C++ executable."""

from __future__ import annotations

from pathlib import Path

import icu
import pins
import toolenv
from toolenv import run

SOURCE = pins.HERE / "cxxproof" / "main.cpp"


def build(with_icu: bool = True) -> Path:
    """Compile `main.cpp` with libc++; `with_icu=False` is the toolchain-only check."""
    wrap = toolenv.wrappers()
    out = pins.OUT / "cxxproof"
    out.parent.mkdir(parents=True, exist_ok=True)
    command = [wrap["zc++"], *toolenv.CODEGEN, "-std=c++20", str(SOURCE)]
    if with_icu:
        command += [f"-I{icu.PREFIX}/include", "-DU_STATIC_IMPLEMENTATION",
                    f"-L{icu.PREFIX}/lib", "-licui18n", "-licuuc", "-licudata"]
    else:
        command += ["-DNO_ICU"]
    command += [*toolenv.LINK, "-s", "-o", str(out)]
    run(command, pins.WORK, "cxxproof compile")
    return out
