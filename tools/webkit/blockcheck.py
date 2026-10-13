#!/usr/bin/env python3
"""Compile a converted rule list with WebKit and block a page with it (issue #845).

The converter (``xui-app/crates/adblock``) is judged against WebKit's own parser here, not
against our model of it: the rule list goes through ``WKUserContentExtensionStoreCompile`` of
the port's ``webkit-smoke`` (its *rules mode*), is read back from the store like a start
would, attached to the page, and a page that requests blocked and unblocked resources is
loaded. One pattern WebKit refuses fails a whole list, so a pass here is the evidence that a
real list works; the numbers (compile time, peak memory, rules) are the ones the issue asked
for.

    python tools/webkit/blockcheck.py --list target/blockcheck/easylist.txt
    python tools/webkit/blockcheck.py --rules target/blockcheck/easylist-0.json --expect-blocked 2
    python tools/webkit/blockcheck.py --list FILE --no-docker       # on a Linux host that has the build

``--list`` converts a filter list first (``cargo run -p lazyadblock --example convert``), then
checks every part. Needs ``target/webkit/bin`` from ``python tools/webkit/port.py`` in a build
whose volume has content extensions on (``OptionsLazyOS.cmake``) and patch 0017 (the blocked
count), and Docker, like the port build.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))

#: A page with two requests the rule lists of EasyList block and one they do not: the title proves
#: the page loaded, the count proves what was blocked. The hosts do not answer in the build
#: container, which does not matter: a blocked request is never made.
DEFAULT_PAGE = (
    "data:text/html,<title>blockcheck</title><p>page</p>"
    "<img src=\"http://ad.doubleclick.net/ad.gif\">"
    "<script src=\"http://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js\"></script>"
    "<img src=\"http://lazyos-blockcheck.invalid/logo.png\">"
)

LINE = re.compile(r"^WEBKIT:BLOCK:(\w+):(.*)$")


def in_container() -> bool:
    return os.environ.get("LAZYOS_JSC_IN_CONTAINER") == "1"


def convert(list_path: Path) -> list[Path]:
    """Runs the converter on the host and returns the rule lists it wrote."""
    out = ROOT / "target" / "blockcheck" / list_path.stem
    manifest = ROOT / "xui-app" / "Cargo.toml"
    done = subprocess.run(
        ["cargo", "run", "--release", "-q", "--manifest-path", str(manifest), "-p", "lazyadblock",
         "--example", "convert", "--", str(list_path), str(out)],
        capture_output=True, text=True)
    sys.stdout.write(done.stdout)
    if done.returncode != 0:
        sys.stderr.write(done.stderr)
        raise SystemExit(f"converting {list_path} failed")
    return sorted(out.parent.glob(f"{out.name}-*.json"))


def run_smoke(rules: Path, page: str) -> tuple[int, str]:
    """Inside the build container: the rules mode of webkit-smoke on `rules`."""
    import port_cmake
    import wkpins
    port_cmake.install_host_layout()
    import tempfile
    env = dict(os.environ, WEBKIT_SMOKE_RULES=str(rules), WEBKIT_SMOKE_TIMEOUT="240")
    with tempfile.TemporaryDirectory(prefix="blockcheck-", dir=wkpins.jsc_pins.WORK,
                                     ignore_cleanup_errors=True) as scratch:
        env["WEBKIT_SMOKE_RULES_STORE"] = str(Path(scratch) / "store")
        done = subprocess.run([str(port_cmake.HOST_BIN / "webkit-smoke"), "about:blank", page],
                              capture_output=True, text=True, timeout=300, cwd=scratch, env=env)
    return done.returncode, done.stdout + done.stderr


def judge(output: str, expect_blocked: int | None) -> list[str]:
    """What is wrong with one run's output (empty: it passed)."""
    facts: dict[str, str] = {}
    for line in output.splitlines():
        found = LINE.match(line.strip())
        if found:
            facts[found.group(1)] = found.group(2)
    problems = []
    compile_fact = facts.get("COMPILE", "")
    if not compile_fact.startswith("0:"):
        problems.append(f"WebKit did not accept the list (COMPILE:{compile_fact or 'missing'})")
    if not facts.get("LOOKUP", "").startswith("0:"):
        problems.append(f"the stored list could not be read back (LOOKUP:{facts.get('LOOKUP', 'missing')})")
    if "WEBKIT:W4:PASS" not in output:
        problems.append("the page did not load and paint (no WEBKIT:W4:PASS)")
    blocked = facts.get("BLOCKED", "")
    count = int(blocked.split(":")[0]) if blocked[:1].isdigit() else -1
    if expect_blocked is not None and count < expect_blocked:
        problems.append(f"expected at least {expect_blocked} blocked loads, saw {count}")
    return problems


def check_in_container(args: argparse.Namespace) -> int:
    status = 0
    for name in args.rules:
        path = Path(name)
        code, output = run_smoke(path, args.page)
        print(f"== {path.name}")
        for line in output.splitlines():
            if line.startswith("WEBKIT:BLOCK:") or line.startswith("WEBKIT:W4:FAIL") or "WEBKIT:W4:PASS" in line:
                print("  " + line)
        problems = judge(output, args.expect_blocked)
        if code != 0 and not problems:
            problems.append(f"webkit-smoke exited {code}")
        for problem in problems:
            print("  FAIL: " + problem)
        print("  " + ("PASS" if not problems else "FAILED"))
        status |= 1 if problems else 0
    return status


def run_in_docker(argv: list[str]) -> int:
    import deps
    build = deps.load_jsc_build()
    import shutil
    if shutil.which("docker") is None:
        print("docker not found (install Docker, or use --no-docker on a Linux host)")
        return 127
    command = ["docker", "run", "--rm", "--entrypoint", "python3",
               "-v", f"{ROOT}:/work", "-v", f"{build.VOLUME}:/build"]
    return subprocess.run([*command, build.IMAGE, "-I", "/work/tools/webkit/blockcheck.py",
                           "--in-container", *argv]).returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--list", help="a filter list to convert first (host: cargo)")
    parser.add_argument("--rules", nargs="*", default=[], help="converted rule lists (JSON)")
    parser.add_argument("--page", default=DEFAULT_PAGE, help="the page to load (a data: URL)")
    parser.add_argument("--expect-blocked", type=int, default=None,
                        help="fail when fewer loads were blocked (default 2 with the default page)")
    parser.add_argument("--no-docker", action="store_true", help="run on this Linux host")
    parser.add_argument("--in-container", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.expect_blocked is None and args.page == DEFAULT_PAGE:
        args.expect_blocked = 2

    if args.in_container or (args.no_docker and in_container()):
        return check_in_container(args)

    rules = [Path(r).resolve() for r in args.rules]
    if args.list:
        rules += convert(Path(args.list).resolve())
    if not rules:
        parser.error("name --list or --rules")
    inside = []
    for path in rules:
        try:
            inside.append("/work/" + path.relative_to(ROOT).as_posix())
        except ValueError:
            parser.error(f"{path} is outside the repository, which the container mounts")
    forwarded = ["--rules", *inside, "--page", args.page]
    if args.expect_blocked is not None:
        forwarded += ["--expect-blocked", str(args.expect_blocked)]
    if args.no_docker:
        return subprocess.run([sys.executable, "-I", str(Path(__file__)), "--in-container", *forwarded]).returncode
    return run_in_docker(forwarded)


if __name__ == "__main__":
    sys.exit(main())
