# LazyWeb harness

`tools/web/run.py` builds a desktop image with the LazyWeb browser
([docs/lazyweb.md](../../docs/lazyweb.md)), browses two sites in it and judges
the run from three independent kinds of evidence: the serial markers, what the
host's servers recorded, and the screenshots.

```bash
python tools/web/run.py                     # build, boot, browse, judge (TCG: allow ~30 min)
python tools/web/run.py --no-build          # the image and certificates of the last run
python tools/web/run.py --precheck-only     # console image, curl only: tests the harness, no browser
python tools/web/run.py --accel none        # force TCG (the default `auto` uses KVM/WHPX)
python tools/web/run.py --live              # the real example.com and theoldnet.com
python tools/web/run.py --live --extra-ca proxy.pem   # behind a TLS-intercepting proxy
python tools/web/test_judge.py              # the judge fails when it should; fixtures are current
python tools/web/gen_fixtures.py [--check]  # redraw the test site's pictures
python tools/web/session.py --write         # regenerate tools/screenshot/examples/lazyweb.json
```

Output (`shots/web/`): `serial.log`, `session.json`, `summary.json`,
`net.pcap` (the guest's traffic), `requests.json` (every request the host
servers answered), `shot_01_example.png`, `shot_02_theoldnet.png`,
`shot_03_theoldnet_later.png`, `shot_11_download.png`, `shot_12_mailto.png`,
`shot_13_history.png`, `shot_14_downloads.png`, `shot_15_menu.png` (the History
menu open), and `certs/` (the run's throwaway CA and leaf;
`--no-build` reuses them). The verdict lines start with `LAZYWEB:` and the
last one is `LAZYWEB:HARNESS:PASS|FAIL`; the exit status follows it.

## The stand-in sites

The sandbox and CI cannot reach example.com or theoldnet.com, and the live
theoldnet.com shows only a maintenance notice, so the host serves both
(`sites.py`) and the image is told they live on the host:

| URL | Served from | |
|---|---|---|
| `http://example.com/` | `127.0.0.1:80` | `fixtures/example.com/index.html`, today's page ("Learn more"); `--example-page classic.html` serves the long-lived earlier one ("More information...") |
| `http://theoldnet.com/...` | `127.0.0.1:80` | `301` to `https://theoldnet.com/...`, like the real site |
| `https://theoldnet.com/`, `https://www.theoldnet.com/` | `127.0.0.1:443` | `fixtures/theoldnet.com/`: a late-90s home page (table layout, `<font>`, `<center>`, a tiled `background=` GIF, a PNG logo with alpha, a JPEG photo, an animated GIF, a rainbow rule, a CSS file, links) |
| `https://en.wikipedia.org/`, `https://upload.wikimedia.org/`, `https://thumb.wikimedia.org/` | `127.0.0.1:443` | `fixtures/wikipedia/`: the Main Page and "1762" in the 2010 Vector skin that LazyWeb asks for, with their style sheets and pictures (`wiki.py`; `wikicapture.py` refreshes them). `http://` redirects to `https://` |

How the guest gets there: QEMU's user network maps the gateway, `10.0.2.2`,
to the host's loopback, so a guest connection to `10.0.2.2:443` reaches
`127.0.0.1:443`. The image is built with `LAZYOS_TLS_TEST_HOSTS` naming a file
with `10.0.2.2 example.com www.example.com theoldnet.com www.theoldnet.com`
(appended to `/etc/hosts`, which musl's resolver reads first) and with
`LAZYOS_TLS_TEST_CA` naming the run's test CA (appended to the system bundle),
which issued the HTTPS server's leaf for `theoldnet.com` and
`www.theoldnet.com`. The browser keeps the real URLs and ports and verifies
the certificate exactly as in production. Such an image trusts a key that
existed only on this machine: never ship it.

Ports 80 and 443 are privileged on Linux: run as root, or once
`sudo sysctl net.ipv4.ip_unprivileged_port_start=80`. The harness refuses to
start when something else holds either port.

The pictures are drawn by `gen_fixtures.py` with the standard library only
(`imgenc.py`: PNG and GIF with LZW and animation; `jpegenc.py`: a baseline
JPEG encoder) and checked in; `--check` (run by `test_judge.py`) keeps them
byte-identical to the generator.

## The session

`session.py` writes the steps (and `tools/screenshot/examples/lazyweb.json`,
the same desktop session):

1. When the network is up, one typed line has the image's `wget` fetch a check
   script from the host and run it. Its `curl` checks print `WEBH:<name>:PASS`:
   `/etc/hosts` has the names, `http://example.com/` is the copy, HTTPS to
   theoldnet.com and www.theoldnet.com verifies, plain HTTP redirects, and the
   PNG, JPEG and GIF come back whole. They prove the path before the browser
   is blamed; their requests carry `?precheck`, so the judge never counts them
   for the browser.
2. LazyWeb is started from the Terminal through `mimed`: the check script
   writes `/tmp/open.rhai` (`sys::mimed::open(<arg>, "open")`, printing
   `OPEN:<mime>:<app>`), and `rhai /tmp/open.rhai http://example.com/` has
   `mimed` guess `x-scheme-handler/http`, pick LazyWeb (its package registers
   the type) and ask `init` to launch it with the URL. Apps `init` launches
   print to the serial log. (`messengerctl` reads the console, not its
   arguments, so it cannot do this from the Terminal.)
3. After `WEB:UP:PASS`, `WEB:LOAD:http://example.com/` and
   `WEB:TITLE:Example Domain`, a screenshot; then **Ctrl+L**, `https://theoldnet.com/`
   and Enter, which must produce `WEB:LOAD:https://theoldnet.com/` and the page's
   title, and two more screenshots (the second one a few seconds later, while
   the animated GIF moves). Then the same for
   `https://en.wikipedia.org/wiki/Main_Page` and `.../wiki/1762`, each with
   its title and a screenshot (`shot_21_wiki_main.png`, `shot_22_wiki_1762.png`).
4. **Ctrl+L** and `https://theoldnet.com/files/oldnet-kit.zip`, which the
   stand-in sends as an attachment: the browser saves it to `~/Downloads`
   (`WEB:DOWNLOAD:START`, then `WEB:DOWNLOAD:DONE:<name>:<bytes>`). Then
   **Ctrl+L** and `mailto:webmaster@theoldnet.com`, which LazyWeb hands to
   the OS (`WEB:LAUNCH:<url>:OK|FAIL`; FAIL is fine on an image without
   Mail), **Ctrl+H** (`WEB:LOAD:about:history`) and **Ctrl+J**
   (`WEB:LOAD:about:downloads`), each with a screenshot.

Assumptions about the browser, to keep in step with `xui-app/web`: it prints
`WEB:UP:PASS` once its window is up, `WEB:LOAD:<url>` for each page it loads,
`WEB:TITLE:<title>` once the page's title is known and `WEB:FAIL:<reason>` on
any failure; it takes the focus when its window opens; **Ctrl+L** focuses and
selects the address field and Enter navigates to it.

## The verdict

| Section | Passes when |
|---|---|
| `SESSION` | every gate of the session opened in time |
| `CHECKS` | every `WEBH:<name>` check printed `PASS` |
| `CHECKS-SEEN` | the host answered each check's request, under its own Host header, SNI naming that host |
| `BROWSER` | `WEB:UP:PASS`, both `WEB:LOAD`s, `WEB:TITLE:Example Domain` and the stand-in's title (`--live`: any other title), no `WEB:FAIL` |
| `SERVERS` | the browser's own requests: `GET /` from example.com over HTTP, TLS to theoldnet.com with SNI matching every Host header, `/` and every picture and style sheet the page references (`judge.page_assets`) |
| `FEATURES` | `mimed` opened `http://example.com/` with LazyWeb (`OPEN:x-scheme-handler/http:os.lazy.lazyweb`), the download completed with the stand-in's exact size and the host served it, the `mailto:` link was handed on, both `about:` pages loaded |
| `SHOTS` | each screenshot has content, the retro page is colourful (24+ colours) and differs from example.com's |
| `WIKIPEDIA` | both Wikipedia pages loaded under the URL typed with the copy's title; the host saw each page asked for with `useskin=vector` (never without), and every style sheet and picture the pages name |
| `WIKI-SHOTS` | both Wikipedia screenshots have content and colour (24+) and differ |

`--precheck-only` judges `SESSION`, `CHECKS` and `CHECKS-SEEN` on a console
image; `--live` drops what only the stand-ins can record (`CHECKS-SEEN`,
`SERVERS`, `FEATURES`, `WIKIPEDIA`, `WIKI-SHOTS`).
