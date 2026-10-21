#!/usr/bin/env python3
"""Web notifications through LazyWeb+ (docs/lazywebplus.md, "Notifications"): build the desktop
image, browse a page that uses the Notifications API in two boots, and judge what the window, the
shell, the OS volume and the host's servers saw.

The image is `run.py`'s (the app built first: ``python tools/webkit/deps.py``, ``port.py`` and
``app.py``, then ``python tools/xui/build.py``). The test page is
``tools/web/fixtures/theoldnet.com/webplus/notify.html``, served over HTTPS from the host with the
run's throwaway CA like the rest of LazyWeb+'s pages.

1. the first boot (``notify_session.allow``): a request with no gesture is refused and opens
   nothing; a click asks in the browser's bar; Allow is kept; a post is a toast of the shell
   titled with the site; the same tag updates it in place; a click runs the page's handler; a burst
   of six is cut short; a click on a toast of a page that has gone brings the page back;
2. the second boot (``notify_session.kept``): the permission is found again (no bar), the other
   origin is asked and blocked and shows nothing, ``about:lazyweb`` lists the decisions and Forget
   removes one;
3. the verdict (``notify_judge.py``): the markers and the shell's POST lines of both boots, the
   page's own titles, ``notify.tsv`` read from the OS volume after each boot.

    python tools/webplus/notify_run.py                # build, boot twice, judge
    python tools/webplus/notify_run.py --no-build     # the image and certificates of the last run
    python tools/webplus/notify_run.py --judge-only DIR

Needs the ports ``run.py`` needs (80 and 443 on 127.0.0.1). Screenshots, logs and the request
records go to ``shots/webplus-notify``; read the ``n*`` and ``k*`` pictures: the bar, the toast with
the site in it. Every run recreates ``target/lazyos.img``'s OS volume.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import run as base  # noqa: E402  (also puts tools/web, tools/abi and this directory on the path)
import certs  # noqa: E402
import notify_judge as n  # noqa: E402
import notify_session  # noqa: E402
import sites  # noqa: E402
from lazygui import limits  # noqa: E402

ROOT = base.ROOT


def read_decisions() -> str | None:
    """`notify.tsv` on the OS volume, or None when the profile has none."""
    data = base.osread("cat", f"{base.APP_DIR}/{n.NOTIFY_FILE}")
    return data.decode(errors="replace") if data is not None else None


def verdict(out: Path, boots: dict, records: tuple, decisions: tuple) -> int:
    """Judge both boots. `boots`: {"allow"|"kept": {"ok", "text"}}; `records`: (first, second)
    request records; `decisions`: (notify.tsv after the first boot, after the second)."""
    problems_of = base.report
    ok = problems_of("SESSION", "the boots ran to their end",
                     [f"the {name} boot's session did not finish (see its summary.json)"
                      for name, boot in boots.items() if not boot["ok"]])
    if "allow" in boots:
        ok = problems_of("ALLOW", "no bar without a gesture, one ask, Allow kept, a toast titled with the site, "
                         "updated in place, a burst cut, clicks reached the page or brought it back",
                         n.judge_allow(boots["allow"]["text"])) and ok
    if "kept" in boots:
        ok = problems_of("KEPT", "the allowed site needed no bar, the blocked one showed nothing, Forget worked",
                         n.judge_kept(boots["kept"]["text"])) and ok
    if len(boots) == 2:
        ok = problems_of("PROFILE", "notify.tsv holds the decisions in the documented form",
                         n.judge_profile(*decisions)) and ok
        ok = problems_of("SERVERS", "both origins served the page over HTTPS", n.judge_servers(*records)) and ok
    ok = problems_of("QUIT", "the boots closed the window and reaped both children",
                     [p for boot in boots.values() for p in base.judge.judge_quit(boot["text"])]) and ok
    print(f"{base.judge.P}:NOTIFY:HARNESS:" + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/webplus-notify")
    parser.add_argument("--no-build", action="store_true", help="reuse the image and certificates of the last run")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory", default=limits.BIG_APP_MEMORY, help="guest RAM (default: the big-app profile's 4G)")
    parser.add_argument("--step-timeout", type=float, default=300.0, help="seconds per gate")
    parser.add_argument("--judge-only", metavar="DIR", help="judge a saved run (DIR/allow, DIR/kept, decisions.json)")
    parser.add_argument("--only", choices=("allow", "kept"), help="run one boot against the image and profile of "
                        "the last run (with --no-build; `kept` needs the allow boot's decision on the volume)")
    args = parser.parse_args(argv)
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    if args.judge_only:
        saved = Path(args.judge_only)
        saved_decisions = json.loads((saved / "decisions.json").read_text(encoding="utf-8"))
        boots = {name: {"ok": True, "text": (saved / name / "serial.log").read_text(errors="replace")}
                 for name in ("allow", "kept")}
        return verdict(saved, boots,
                       (base.load_record(saved / "allow" / "requests.json"),
                        base.load_record(saved / "kept" / "requests.json")),
                       (saved_decisions["first"], saved_decisions["second"]))
    out.mkdir(parents=True, exist_ok=True)
    problem = base.built()
    if problem:
        print(f"{base.judge.P}:NOTIFY:HARNESS:FAIL {problem}")
        return 1
    busy = sites.ports_free((sites.HTTP_PORT, sites.HTTPS_PORT, base.judge.CLOSER_PORT))
    if busy:
        print(f"{base.judge.P}:NOTIFY:HARNESS:FAIL host port(s) {busy} in use: {sites.port_hint(OSError('busy'))}")
        return 1
    files = certs.load(out / "certs") if args.no_build else certs.generate(out / "certs")
    if files is None or (args.no_build and not base.IMAGE.is_file()):
        print(f"{base.judge.P}:NOTIFY:HARNESS:FAIL --no-build needs the image and certificates of an earlier run")
        return 1
    if not args.no_build:
        error = base.build_image({"LAZYOS_TLS_TEST_CA": str(files.ca), "LAZYOS_TLS_TEST_HOSTS": str(files.hosts)},
                                 False)
        if error:
            print(f"{base.judge.P}:NOTIFY:HARNESS:FAIL {error}")
            return 1
    try:
        served = sites.Sites(files.leaf_cert, files.leaf_key, closer_port=base.judge.CLOSER_PORT)
    except OSError as error:
        print(f"{base.judge.P}:NOTIFY:HARNESS:FAIL cannot start the host servers: {sites.port_hint(error)}")
        return 1
    boots: dict = {}
    try:
        if args.only != "kept":
            ok, text = base.run_session(notify_session.allow(args.step_timeout), out / "allow", args)
            boots["allow"] = {"ok": ok, "text": text}
        record_allow = served.snapshot()
        first = read_decisions()
        if args.only != "allow":
            ok, text = base.run_session(notify_session.kept(args.step_timeout), out / "kept", args)
            boots["kept"] = {"ok": ok, "text": text}
        all_record = served.snapshot()
        second = read_decisions()
    finally:
        served.close()
    record_kept = sites.Record(all_record.requests[len(record_allow.requests):],
                               all_record.handshakes[len(record_allow.handshakes):])
    if len(boots) == 2:
        # A partial run (--only) must not overwrite the record --judge-only reads.
        base.save_record(out / "allow" / "requests.json", record_allow)
        base.save_record(out / "kept" / "requests.json", record_kept)
        (out / "decisions.json").write_text(json.dumps({"first": first, "second": second}, indent=1),
                                            encoding="utf-8")
    return verdict(out, boots, (record_allow, record_kept), (first, second))


if __name__ == "__main__":
    sys.exit(main())
