#!/usr/bin/env python3
"""The guest sessions of the LazyWeb harness (`run.py`).

Both start the same way: once the network is up, one short typed line has the
image's own `wget` (the TLS work's `fetch`) download a check script from the
host's plain server and run it. The script's `curl` checks (`WEBH:<name>:`)
prove the path the browser will take: /etc/hosts sends the sites' names to
the host, the test CA verifies theoldnet.com's certificate, plain HTTP
redirects to HTTPS, pictures come back whole. Its requests carry `?precheck`
so the judge never mistakes them for the browser's. Typing one line instead
of every check avoids keystrokes lost on a busy TCG guest.

* **desktop** (the full run): the checks in the Terminal (their output sent
  to `/dev/console`, since the Terminal reports only a command's first line on
  serial), then `rhai /tmp/open.rhai http://example.com/` (a script the
  check script writes: `sys::mimed::open`), which has `mimed` pick LazyWeb
  for `x-scheme-handler/http` and `init` launch it with the URL.
  Once that page reports its title, **Ctrl+L** reaches the address field for
  `https://theoldnet.com/`, then for the copies of two Wikipedia pages
  (`wiki.py`); then the same for the site's download (saved to
  ~/Downloads) and a `mailto:` link handed back to the OS, **Ctrl+H** for
  the history page and **Ctrl+J** for the downloads page. The browser's
  window is assumed to take the focus when it opens.
* **console** (`run.py --precheck-only`): the checks alone on a console
  image, which needs no browser: a test of the harness itself.

    python tools/web/session.py --write   # regenerate tools/screenshot/examples/lazyweb.json
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import judge  # noqa: E402
import wiki  # noqa: E402

EXAMPLE = ROOT / "tools" / "screenshot" / "examples" / "lazyweb.json"
#: Where the host's plain server hands out the check script (any Host).
SCRIPT_PATH = "/lazyweb-check.sh"
#: The host as the guest sees it on QEMU's user network.
GATEWAY = "10.0.2.2"
TYPE_DELAY = 0.05
#: Opens its argument through `mimed`, as any app or script would, and prints
#: `OPEN:<mime>:<app>`. (`messengerctl` is interactive and reads the console,
#: so it cannot be given a command from the Terminal.)
OPEN_SCRIPT = "/tmp/open.rhai"
OPENER = 'let r = sys::mimed::open(os::args()[0], "open"); print("OPEN:" + r.mime + ":" + r.app)'
HELPER = 'ok() { if grep -q "$1"; then echo $m:$2:PASS; else echo $m:$2:FAIL; fi; }'


def checks(live: bool = False) -> list[tuple[str, str]]:
    """(name, command) of every check the script runs. `live` checks the
    real sites, which the host does not record (no `?precheck` needed)."""
    if live:
        return [("http", "curl -s http://example.com/ | ok 'Example Domain' http"),
                ("https", "curl -s -o /dev/null -w '%{http_code}' https://theoldnet.com/ "
                          "| ok '^[23]' https")]
    tag = f"?{judge.PRECHECK_TAG}"
    return [
        ("hosts", "grep -c theoldnet.com /etc/hosts | ok '^1$' hosts"),
        ("http", f"curl -s 'http://example.com/{tag}' | ok '{judge.EXAMPLE_TITLE}' http"),
        ("https", f"curl -s 'https://theoldnet.com/{tag}' | ok '{judge.OLDNET_TITLE}' https"),
        ("redirect", f"curl -sI 'http://theoldnet.com/{tag}' | ok 'https://theoldnet.com/' redirect"),
        ("www", f"curl -s 'https://www.theoldnet.com/style.css{tag}' | ok 'TheOldNet' www"),
        ("png", f"curl -s 'https://theoldnet.com/images/logo.png{tag}' | ok PNG png"),
        ("jpeg", f"curl -s 'https://theoldnet.com/images/photo.jpg{tag}' | ok JFIF jpeg"),
        ("gif", f"curl -s 'https://theoldnet.com/images/construction.gif{tag}' | ok GIF89a gif"),
        ("wiki", f"curl -s '{_tagged(wiki.ARTICLE_URL + '?useskin=vector')}' "
                 "| ok '<title>1762' wiki"),
        ("thumb", f"curl -s '{_tagged(wiki.wiki_picture())}' | wc -c "
                  f"| ok '^ *{len(wiki.picture_bytes(wiki.wiki_picture()))}$' thumb"),
    ]


def _tagged(url: str) -> str:
    """`url` with the harness's query parameter first (the copies ignore it)."""
    path, _, query = url.partition("?")
    return f"{path}?{judge.PRECHECK_TAG}" + (f"&{query}" if query else "")


def body(items: list[tuple[str, str]]) -> bytes:
    """The check script the guest downloads and runs."""
    lines = ["echo WEBH:started", "m=WEBH", HELPER, f"echo '{OPENER}' > {OPEN_SCRIPT}",
             *(command for _, command in items), "echo $m:done"]
    return ("\n".join(lines) + "\n").encode()


def bootstrap(console: bool) -> str:
    line = f"wget -q -O /tmp/w.sh http://{GATEWAY}{SCRIPT_PATH}?{judge.PRECHECK_TAG}; sh /tmp/w.sh"
    return line if console else line + " >/dev/console 2>&1"


def _check_steps(items, console: bool, step_timeout: float) -> list[dict]:
    steps: list[dict] = [{"at": 2.0, "type": bootstrap(console), "delay": TYPE_DELAY},
                         {"key": "enter", "until": "WEBH:started", "timeout": step_timeout}]
    steps += [{"wait_for": f"WEBH:{name}:", "timeout": step_timeout} for name, _ in items]
    return steps + [{"wait_for": "WEBH:done", "timeout": step_timeout}]


def _title(title: str | None) -> dict:
    """A gate on the second page's title (any but example.com's when live)."""
    if title is None:
        return {"wait_for": f"WEB:TITLE:(?!{judge.EXAMPLE_TITLE}).", "regex": True}
    return {"wait_for": f"WEB:TITLE:{title}"}


def _address(url: str, until: str, step_timeout: float) -> list[dict]:
    """Ctrl+L, `url`, Enter, until `until` is on serial."""
    return [{"key_down": "ctrl"}, {"key": "l"}, {"key_up": "ctrl"},
            {"wait": 1.0},
            {"type": url, "delay": TYPE_DELAY},
            {"key": "enter", "until": until, "timeout": step_timeout}]


def _shortcut(key: str, until: str, step_timeout: float) -> list[dict]:
    """Ctrl+`key`, until `until` is on serial."""
    return [{"key_down": "ctrl"}, {"key": key, "until": until, "timeout": step_timeout},
            {"key_up": "ctrl"}]


def _browser_steps(step_timeout: float, title: str | None) -> list[dict]:
    opener = f"rhai {OPEN_SCRIPT} {judge.EXAMPLE_URL} >/dev/console 2>&1"
    return [
        {"at": 1.0, "type": opener, "delay": TYPE_DELAY, "phase": "app"},
        {"key": "enter", "until": "WEB:UP:PASS", "timeout": step_timeout},
        {"wait_for": f"WEB:LOAD:{judge.EXAMPLE_URL}", "timeout": step_timeout},
        {"wait_for": f"WEB:TITLE:{judge.EXAMPLE_TITLE}", "timeout": step_timeout},
        {"at": 5.0, "shot": "01_example"},
        *_address(judge.OLDNET_URL, f"WEB:LOAD:{judge.OLDNET_URL}", step_timeout),
        {**_title(title), "timeout": step_timeout},
        # Time for the pictures, then a second look (the animated GIF moves).
        {"at": 10.0, "shot": "02_theoldnet"},
        {"at": 2.0, "shot": "03_theoldnet_later"},
    ]


def _wiki_steps(step_timeout: float) -> list[dict]:
    """The Wikipedia copies (issue #632): the Main Page, then an article."""
    steps: list[dict] = []
    for url, shot in ((wiki.MAIN_URL, "21_wiki_main"), (wiki.ARTICLE_URL, "22_wiki_1762")):
        steps += [*_address(url, f"WEB:LOAD:{url}", step_timeout),
                  {"wait_for": f"WEB:TITLE:{wiki.title(url)}", "timeout": step_timeout},
                  # Time for the style sheets and pictures.
                  {"at": 15.0, "shot": shot}]
    return steps


def _feature_steps(step_timeout: float) -> list[dict]:
    """The download, a `mailto:` link, the history and downloads pages."""
    download = f"https://theoldnet.com{judge.DOWNLOAD_PATH}"
    return [
        *_address(download, "WEB:DOWNLOAD:START:", step_timeout),
        {"wait_for": "WEB:DOWNLOAD:DONE:", "timeout": step_timeout},
        {"at": 2.0, "shot": "11_download"},
        *_address(judge.MAILTO_URL, "WEB:LAUNCH:", step_timeout),
        {"at": 2.0, "shot": "12_mailto"},
        *_shortcut("h", "WEB:LOAD:about:history", step_timeout),
        {"at": 8.0, "shot": "13_history"},
        *_shortcut("j", "WEB:LOAD:about:downloads", step_timeout),
        {"at": 8.0, "shot": "14_downloads"},
        # The History menu, to see its icons: the pointer rests at the
        # screen's centre, the menu title is at (249, 147) on a 720p desktop.
        {"at": 1.0, "mouse_move": [-391, -216]},
        {"at": 1.0, "mouse_down": "left"},
        {"at": 0.4, "mouse_up": "left"},
        {"at": 4.0, "shot": "15_menu"},
        {"key": "esc"},
    ]


def script(console: bool = False, live: bool = False, step_timeout: float = 300.0) -> list[dict]:
    """The `qemu_session.py` steps for one run (see the module docstring)."""
    items = checks(live)
    if console:
        steps = [{"wait_for": "/ #", "timeout": 300}]
    else:
        steps = [{"wait_for": "PKGD:PROVISION:DONE", "timeout": 600},
                 {"wait_for": "TERM:UP:PASS", "timeout": 300}]
    steps.append({"wait_for": "NETD:RESOLV wrote", "timeout": 300})
    steps += _check_steps(items, console, step_timeout)
    if console:
        steps.append({"at": 1.0, "shot": "checks"})
    else:
        steps += _browser_steps(step_timeout, None if live else judge.OLDNET_TITLE)
        if not live:
            steps += _wiki_steps(step_timeout) + _feature_steps(step_timeout)
    return steps + [{"at": 1.0, "quit": True}]


def render(steps: list[dict]) -> str:
    """One step per line, like the hand-written session scripts."""
    return "[\n" + ",\n".join("  " + json.dumps(step) for step in steps) + "\n]\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--write", action="store_true", help=f"rewrite {EXAMPLE.relative_to(ROOT)}")
    args = parser.parse_args(argv)
    text = render(script())
    if args.write:
        EXAMPLE.write_text(text, encoding="utf-8")
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
