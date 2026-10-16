#!/usr/bin/env python3
"""The NetworkProcess's descriptors stay bounded with a full HTTP cache (issue #892).

Patch 0019 once kept the descriptor of every file the curl backend's ``NetworkCache::Data`` mapped,
so a NetworkProcess holding cache entries held one descriptor each, on the way to ``fd_max``. This
runs the port's ``webkit-smoke`` in its *cache mode* (``webkit/port/smoke/cache.cpp``) twice on
one cache directory against a page with ``--pictures`` cacheable pictures served from this
container: the first run fills the cache, the second reads every picture back from it (the server
must see almost none of them again) and ends by counting the NetworkProcess's descriptors
(``/proc/<pid>/fd``). The judge fails when more than ``--max-cache-fds`` are open below the cache.

    python tools/webkit/cachecheck.py                  # Docker, like the port build
    python tools/webkit/cachecheck.py --no-docker      # on a Linux host that has the build

Needs ``target/webkit/bin`` from ``python tools/webkit/port.py``.
"""

from __future__ import annotations

import argparse
import http.server
import os
import re
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))

#: Bigger than the cache's inline body limit (16 KiB): each picture is a blob file of its own, the
#: kind ``mapFile`` maps when the second run reads it back.
PICTURE_BYTES = 32 * 1024
FDS = re.compile(r"^WEBKIT:CACHE:FDS:total=(\d+):cache=(\d+)$")


def in_container() -> bool:
    return os.environ.get("LAZYOS_JSC_IN_CONTAINER") == "1"


class Pages(http.server.BaseHTTPRequestHandler):
    """A page of ``count`` pictures, each cacheable for a day; counts the picture requests."""

    count = 0
    pictures_served = 0
    lock = threading.Lock()

    def do_GET(self) -> None:  # noqa: N802 (the handler's API)
        if self.path == "/":
            tags = "".join(f'<img src="/p/{n}.bin" width="4" height="4">' for n in range(self.count))
            self.reply(f"<title>cachecheck</title><p>pictures</p>{tags}".encode(), "text/html")
        elif self.path.startswith("/p/"):
            with Pages.lock:
                Pages.pictures_served += 1
            seed = self.path.encode()
            self.reply((seed * (PICTURE_BYTES // len(seed) + 1))[:PICTURE_BYTES], "application/octet-stream")
        else:
            self.send_error(404)

    def reply(self, body: bytes, kind: str) -> None:
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "public, max-age=86400")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args: object) -> None:
        pass


def network_processes() -> list[Path]:
    """The ``/proc`` entries of the NetworkProcesses running now."""
    found = []
    for proc in Path("/proc").iterdir():
        try:
            # The image's name (`networkprocess`, libs/fhs) or the build's.
            if proc.name.isdigit() and os.readlink(proc / "exe").lower().endswith("networkprocess"):
                found.append(proc)
        except OSError:
            continue  # gone, or not ours to look at
    return found


def cache_descriptors(processes: list[Path], prefix: str) -> int:
    """The most descriptors one of ``processes`` has open below ``prefix`` right now."""
    most = 0
    for proc in processes:
        try:
            count = 0
            for fd in os.scandir(proc / "fd"):
                try:
                    count += os.readlink(fd.path).startswith(prefix)
                except OSError:
                    continue  # closed meanwhile
        except OSError:
            continue
        most = max(most, count)
    return most


def run_smoke(url: str, cache: Path, scratch: str) -> tuple[int, str, int]:
    """One run; its output and the most cache descriptors the NetworkProcess held during it."""
    import port_cmake
    env = dict(os.environ, WEBKIT_SMOKE_CACHE=str(cache), WEBKIT_SMOKE_TIMEOUT="240")
    child = subprocess.Popen([str(port_cmake.HOST_BIN / "webkit-smoke"), "about:blank", url],
                             stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, cwd=scratch,
                             env=env)
    peak = 0
    done = threading.Event()

    def sample() -> None:
        # As often as it can: a mapped entry may be held for a few milliseconds only.
        nonlocal peak
        prefix = str(cache / "cache") + "/"
        processes: list[Path] = []
        rounds = 0
        while not done.is_set():
            if rounds % 50 == 0:
                processes = network_processes()
            rounds += 1
            peak = max(peak, cache_descriptors(processes, prefix))
            if not processes:
                done.wait(0.01)

    sampler = threading.Thread(target=sample, daemon=True)
    sampler.start()
    output, _ = child.communicate(timeout=300)
    done.set()
    sampler.join()
    return child.returncode, output, peak


def judge(first: str, second: str, served_again: int, count: int, max_fds: int) -> list[str]:
    """What is wrong with the two runs (empty: they passed)."""
    problems = []
    for name, output in (("first", first), ("second", second)):
        if "WEBKIT:W4:PASS" not in output:
            problems.append(f"the {name} run did not load and paint the page (no WEBKIT:W4:PASS)")
    if served_again > count // 2:
        problems.append(f"the second run fetched {served_again} of {count} pictures again: the cache was not used")
    found = [FDS.match(line.strip()) for line in second.splitlines()]
    counts = [m for m in found if m]
    if not counts:
        problems.append("the second run printed no WEBKIT:CACHE:FDS line")
    elif int(counts[-1].group(2)) > max_fds:
        problems.append(f"the NetworkProcess holds {counts[-1].group(2)} descriptors below the cache "
                        f"(at most {max_fds}) after reading {count} entries back")
    return problems


def check_in_container(args: argparse.Namespace) -> int:
    import port_cmake
    import wkpins
    port_cmake.install_host_layout()
    Pages.count = args.pictures
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Pages)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{server.server_address[1]}/"
    with tempfile.TemporaryDirectory(prefix="cachecheck-", dir=wkpins.jsc_pins.WORK,
                                     ignore_cleanup_errors=True) as scratch:
        cache = Path(scratch) / "profile"
        code1, first, peak1 = run_smoke(url, cache, scratch)
        served_first = Pages.pictures_served
        code2, second, peak2 = run_smoke(url, cache, scratch)
        served_again = Pages.pictures_served - served_first
        blobs = sum(1 for path in (cache / "cache").rglob("*") if "Blobs" in path.parts and path.is_file())
        records = sum(1 for path in (cache / "cache").rglob("*") if "Records" in path.parts and path.is_file())
    server.shutdown()
    print(f"  cache: {records} record files, {blobs} blob files")
    for line in (first + second).splitlines():
        if line.startswith(("WEBKIT:CACHE:", "WEBKIT:W4:FAIL")) or "WEBKIT:W4:PASS" in line:
            print("  " + line)
    print(f"  pictures served: first run {served_first}, second run {served_again}")
    print(f"  most descriptors below the cache during a run: first {peak1}, second {peak2}")
    problems = judge(first, second, served_again, args.pictures, args.max_cache_fds)
    if max(peak1, peak2) > args.max_cache_fds:
        problems.append(f"the NetworkProcess held up to {max(peak1, peak2)} descriptors below the cache "
                        f"during a run (at most {args.max_cache_fds})")
    if (code1 or code2) and not problems:
        problems.append(f"webkit-smoke exited {code1} and {code2}")
    for problem in problems:
        print("  FAIL: " + problem)
    print("CACHECHECK:" + ("PASS" if not problems else "FAIL"))
    return 1 if problems else 0


def run_in_docker(argv: list[str]) -> int:
    import deps
    import shutil
    build = deps.load_jsc_build()
    if shutil.which("docker") is None:
        print("docker not found (install Docker, or use --no-docker on a Linux host)")
        return 127
    command = ["docker", "run", "--rm", "--entrypoint", "python3",
               "-v", f"{ROOT}:/work", "-v", f"{build.VOLUME}:/build"]
    return subprocess.run([*command, build.IMAGE, "-I", "/work/tools/webkit/cachecheck.py",
                           "--in-container", *argv]).returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--pictures", type=int, default=300, help="cacheable pictures on the page")
    parser.add_argument("--max-cache-fds", type=int, default=8,
                        help="descriptors below the cache the NetworkProcess may hold at the end")
    parser.add_argument("--no-docker", action="store_true", help="run on this Linux host")
    parser.add_argument("--in-container", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.in_container or (args.no_docker and in_container()):
        return check_in_container(args)
    forwarded = ["--pictures", str(args.pictures), "--max-cache-fds", str(args.max_cache_fds)]
    if args.no_docker:
        return subprocess.run([sys.executable, "-I", str(Path(__file__)), "--in-container",
                               *forwarded]).returncode
    return run_in_docker(forwarded)


if __name__ == "__main__":
    sys.exit(main())
