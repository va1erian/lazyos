#!/usr/bin/env python3
"""Build LazyWeb into a desktop image, browse two sites in it, and judge what
the guest, the host's servers and the screen saw (docs/lazyweb.md).

1. A throwaway test CA and a leaf for theoldnet.com (`certs.py`); the image is
   built with `LAZYOS_LAZYWEB=1` (desktop, `netd`, the HTTPS tools), the CA
   appended to the system bundle (`LAZYOS_TLS_TEST_CA`) and the sites' names
   mapped to the host in /etc/hosts (`LAZYOS_TLS_TEST_HOSTS`): the browser
   verifies certificates exactly as in production.
2. The host serves stand-ins for the two sites on their real ports
   (`sites.py`): http://example.com/ (a faithful copy of the real page) and
   https://theoldnet.com/ (a retro home page with PNG, JPEG, GIF, an animated
   GIF and a style sheet; http:// redirects to it).
3. The session (`session.py`, also `tools/screenshot/examples/lazyweb.json`)
   runs the harness's own `curl` checks, starts LazyWeb at
   http://example.com/, then goes to https://theoldnet.com/ through the
   address bar, with a screenshot of each page.
4. The verdict (`judge.py`): the checks and the browser's markers passed; the
   servers saw the browser's requests (Host headers, SNI theoldnet.com, the
   page and every picture); the screenshots show two different pages.

    python tools/web/run.py                    # build, boot, judge (TCG: allow ~30 min)
    python tools/web/run.py --no-build         # reuse the image this script built
    python tools/web/run.py --precheck-only    # console image, no browser: the harness itself
    python tools/web/run.py --live             # the real sites (needs internet; no CI)
    python tools/web/run.py --live --extra-ca proxy.pem   # behind a TLS-intercepting proxy

The servers need ports 80 and 443 on 127.0.0.1 (root, or on Linux
`sudo sysctl net.ipv4.ip_unprivileged_port_start=80`). Exit status is
non-zero on any failure; logs, screenshots and the capture go to `shots/web`.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "abi"))
import busybox  # noqa: E402
import certs  # noqa: E402
import judge  # noqa: E402
import session  # noqa: E402
import sites  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
LAZYWEB_ELF = ROOT / "target" / "xui" / "xui-lazyweb.elf"
#: Switches a previous build may have left in the caller's environment.
STALE = ("LAZYOS_TLS_TEST_CA", "LAZYOS_TLS_TEST_HOSTS", "LAZYOS_DESKTOP", "LAZYOS_CLI",
         "LAZYOS_LAZYWEB", "LAZYOS_XUI_APPS", "LAZYOS_XUI_AUTOSTART")


def _tool(*argv: str) -> bool:
    return subprocess.call([PY, *argv], cwd=ROOT) == 0


def build(env_extra: dict[str, str], console: bool) -> str | None:
    """Build the tools, the browser and the image; an error message, or None."""
    if not _tool(str(ROOT / "tools" / "nettls" / "build.py"), "--require"):
        return "tools/nettls/build.py --require failed (it needs the x86_64-unknown-linux-musl target)"
    shell = busybox.ensure_busybox()
    if shell is None:
        return "no BusyBox for the shell (see tools/abi/busybox.py)"
    env = {k: v for k, v in os.environ.items() if k not in STALE}
    env.update(LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0", LAZYOS_TLS="1", LAZYOS_RESET_OS="1",
               LAZYOS_BUSYBOX=str(shell), **env_extra)
    if console:
        env["LAZYOS_CLI"] = "1"
    else:
        if not _tool(str(ROOT / "tools" / "xui" / "build.py")):
            return "tools/xui/build.py failed"
        if not LAZYWEB_ELF.is_file():
            return (f"{LAZYWEB_ELF} was not built (NetSurf needs zig: pip install ziglang==0.16.0); "
                    "--precheck-only tests the harness without it")
        env.update(LAZYOS_DESKTOP="1", LAZYOS_LAZYWEB="1")
    switches = " ".join(f"{k}={env[k]}" for k in sorted(env) if k.startswith("LAZYOS_")
                        and k != "LAZYOS_BUSYBOX")
    print(f"web: {switches} cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        return "cargo build failed:\n" + result.stderr[-4000:]
    return None if IMAGE.is_file() else f"{IMAGE} was not built"


def run_session(steps: list[dict], out: Path, args) -> tuple[bool, str]:
    """Drive the guest; (session ok, serial log text)."""
    script = out / "session.json"
    script.write_text(session.render(steps), encoding="utf-8")
    command = [PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"), "--image", str(IMAGE),
               "--out", str(out), "--script", str(script), "--accel", args.accel,
               "--net", "--net-forward", "none", "--net-pcap", str(out / "net.pcap"),
               "--fail-on", "PANIC", "--fail-on", "EXCEPTION"]
    if args.qemu:
        command += ["--qemu", args.qemu]
    if args.memory:
        command += ["--memory", args.memory]
    code = subprocess.call(command, cwd=ROOT, stdout=subprocess.DEVNULL)
    log = out / "serial.log"
    return code == 0, log.read_text(errors="replace") if log.is_file() else ""


def report(section: str, detail: str, problems: list[str]) -> bool:
    for problem in problems[:16]:
        print(f"LAZYWEB:{section}:FAIL {problem}")
    if not problems:
        print(f"LAZYWEB:{section}:PASS {detail}".rstrip())
    return not problems


def shots(out: Path) -> list[Path]:
    """The page screenshots the session took, in order (`shot_0*`)."""
    return sorted(out.glob("shot_0*.png"))


def prepare(args, out: Path) -> certs.Files | str:
    """The run's certificates and image; the files, or an error message."""
    files = certs.load(out / "certs") if args.no_build else None
    if files is None:
        if args.no_build:
            return "--no-build needs the certificates of the run that built the image"
        files = certs.generate(out / "certs")
    if args.no_build:
        return files if IMAGE.is_file() else f"{IMAGE} not found"
    extra = {}
    if args.live:
        if args.extra_ca:
            extra["LAZYOS_TLS_TEST_CA"] = str(Path(args.extra_ca).resolve())
    else:
        extra = {"LAZYOS_TLS_TEST_CA": str(files.ca), "LAZYOS_TLS_TEST_HOSTS": str(files.hosts)}
    error = build(extra, args.precheck_only)
    return error if error else files


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/web")
    parser.add_argument("--no-build", action="store_true", help="reuse the image (built by this script)")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory")
    parser.add_argument("--step-timeout", type=float, default=300.0, help="seconds per gate")
    parser.add_argument("--precheck-only", action="store_true",
                        help="a console image and the curl checks only: no browser needed")
    parser.add_argument("--live", action="store_true", help="the real sites instead of the host's")
    parser.add_argument("--extra-ca", metavar="PEM", help="--live: also trust this CA (a TLS proxy)")
    parser.add_argument("--example-page", choices=["index.html", "classic.html"], default="index.html",
                        help="the example.com copy to serve: today's page, or the classic one "
                             "(\"More information...\")")
    args = parser.parse_args(argv)
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    out.mkdir(parents=True, exist_ok=True)
    for stale in ["serial.log", "net.pcap", "summary.json", *(p.name for p in out.glob("shot_*.png"))]:
        (out / stale).unlink(missing_ok=True)
    busy = sites.ports_free()
    if busy:
        print(f"LAZYWEB:HARNESS:FAIL host port(s) {busy} in use: "
              f"{sites.port_hint(OSError('busy'))}")
        return 1
    files = prepare(args, out)
    if isinstance(files, str):
        print(f"LAZYWEB:HARNESS:FAIL {files}")
        return 1
    items = session.checks(args.live)
    try:
        served = sites.Sites(files.leaf_cert, files.leaf_key, args.example_page,
                             (session.SCRIPT_PATH, session.body(items)))
    except OSError as error:
        print(f"LAZYWEB:HARNESS:FAIL cannot start the host servers: {sites.port_hint(error)}")
        return 1
    steps = session.script(args.precheck_only, args.live, args.step_timeout)
    try:
        session_ok, text = run_session(steps, out, args)
    finally:
        record = served.snapshot()
        served.close()
    (out / "requests.json").write_text(json.dumps([vars(r) for r in record.requests], indent=1))
    return verdict(args, out, session_ok, text, record, items)


def verdict(args, out: Path, session_ok: bool, text: str, record, items) -> int:
    ok = report("SESSION", "", [] if session_ok else ["the session did not finish (see summary.json)"])
    names = [name for name, _ in items]
    ok = report("CHECKS", f"{len(names)} curl checks", judge.judge_prechecks(text, names)) and ok
    if not args.live:
        ok = report("CHECKS-SEEN", f"{len(judge.PRECHECK_REQUESTS)} requests as sent",
                    judge.judge_precheck_servers(record)) and ok
    if not args.precheck_only:
        title = None if args.live else judge.OLDNET_TITLE
        ok = report("BROWSER", "up, both pages loaded with their titles",
                    judge.judge_serial(text, title)) and ok
        if not args.live:
            assets = judge.page_assets()
            ok = report("SERVERS", f"the page and {len(assets)} resources over HTTPS, SNI "
                        "theoldnet.com", judge.judge_servers(record, assets)) and ok
        pictures = shots(out)
        ok = report("SHOTS", ", ".join(p.name for p in pictures), judge.judge_shots(pictures)) and ok
    print("LAZYWEB:HARNESS:" + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
