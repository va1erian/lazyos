# LazyWeb, the web browser

LazyWeb (`os.lazy.lazyweb`) is LazyOS's web browser: the
[Blitz](https://github.com/DioxusLabs/blitz) engine (html5ever, Servo's Stylo
for CSS including grid, flexbox, floats and `var()`, Taffy for layout, Parley
for text, `vello_cpu` for painting; PNG, JPEG, GIF, WebP and SVG; no
JavaScript) behind xui's `xui-blitz` web view, in a desktop window of the
`xuid` compositor, fetching pages over `http://` and `https://`. It replaced
NetSurf (issue #649). HTTPS uses the TLS work of
[tls-plan.md](tls-plan.md): rustls with the pure-Rust `nettls-crypto`
provider, certificates checked against the system bundle at
`/etc/ssl/certs/ca-certificates.crt`, names resolved by musl through
`/etc/hosts` and `/etc/resolv.conf`. TLS runs in the browser's own process; no
service sees its traffic.

Where it lives:

| Piece | Where |
|---|---|
| The app (crate `lazyweb`) | `xui-app/web`, a static musl xui client in pure Rust, built with the other apps by `tools/xui/build.py` into `target/xui/xui-lazyweb.elf` (no zig) |
| The core package | `xui-app/packages/lazyweb` (manifest, icons, help), packed by `tools/xui/core_packages.py`; `pkgd` installs it into `/apps` at boot |
| The image switch | `LAZYOS_LAZYWEB=1` (`build_support/lazyweb_embed.rs`), which needs `LAZYOS_DESKTOP=1` and `LAZYOS_NETD=1` and fails the build, saying what to run, when the browser is not built |
| The front ends | `python tools/run_demo.py --lazyweb`; the launcher's *LazyWeb browser* (Simple tab, Network) and the raw switch (Advanced tab, Networking) |
| The harness | `tools/web/run.py` ([tools/web/README.md](../tools/web/README.md)) |

## Licence

NetSurf was licensed GPL-2.0-only, so LazyWeb, which linked it, is still
declared **GPL-2.0-only** (`license = "GPL-2.0-only"` in its `Cargo.toml`).
Blitz and its tree are MIT or Apache-2.0 (Stylo is MPL-2.0, which is
GPL-compatible) and the Liberation fonts are SIL OFL 1.1, so the browser no
longer has to be GPL-2.0-only; whether to relicense it is open (issue #649),
and until it is decided the rule below stands: everything linked into the
browser binary must be available under a GPLv2-compatible licence: MIT, BSD, ISC, Zlib, Apache-2.0 *only alongside*
one of those (Apache-2.0 alone is not GPLv2-compatible), or the GPLv2 itself.
That is why the TLS stack avoids `ring` and `aws-lc` (§3.2 of
[tls-plan.md](tls-plan.md)), and why GPL-3.0 code cannot be linked: LazyOS's
own crates are GPL-3.0-or-later, so any that the browser links must be
offered under a GPLv2-compatible licence as well. `python
tools/nettls/licenses.py` is the gate for the TLS crates; the browser's own
dependency tree needs the same check. The package's help page states the
licence; whoever distributes an image must also offer the corresponding source
of LazyWeb (the revisions the build pins); the Liberation fonts ship with
their licence.

## Running it

```bash
python tools/run_demo.py --lazyweb     # builds the browser and the desktop, boots it
```

`--lazyweb` implies `--desktop`, `--net` (a virtio-net card on QEMU's user
network, which reaches the internet through the host) and `--tls` (`curl`,
`wget` and `fetch` beside the browser). The browser is pure Rust and needs no
zig: `python tools/xui/build.py` builds it with the other apps, and
`run_demo.py` runs that when `target/xui/xui-lazyweb.elf` is missing. Open LazyWeb from the
desktop's menu (Settings -> Menu offers it, like every core package), or from
the Terminal by opening a URL:

```sh
rhai -e 'sys::mimed::open("http://example.com/", "open")'
```

By hand: `LAZYOS_DESKTOP=1 LAZYOS_NETD=1 LAZYOS_NETD_ARGS=demo=0 LAZYOS_TLS=1
LAZYOS_LAZYWEB=1 cargo build`.

The package's permissions (display, input, `netd`'s stack interface and
outbound sockets) are modelled on Net Tools and still to be derived from a run
under its label: build with `LAZYOS_LABEL_TRACE=1`, browse, and read the
`LABEL:DENY` lines ([packages.md](packages.md)).

## The window

* **Menu bar** (Lucide icons, like Mail): File (Open Location, Save Page As
  Download, Close Window), View (Reload, Stop), History (Back, Forward, Home,
  Show All History, Clear History), Downloads (Show Downloads), Help (About).
* **Toolbar**: icon buttons with tooltips for Back, Forward, Reload (which
  turns into Stop while a page loads) and Home, the address field, Go, and a
  throbber that spins while a page loads.
* **Status bar**: the link under the pointer or what the engine is doing, the
  running downloads with a progress bar, and a padlock: closed over HTTPS,
  open over plain HTTP.
* **Keys**: Ctrl+L address bar, Enter go, Esc stop, F5 reload, Alt+Left and
  Alt+Right back and forward, Alt+Home home, Ctrl+H history, Ctrl+J
  downloads, Ctrl+S save the page as a download, Ctrl+W close.

## History and downloads

Every page that finishes loading over `http:`, `https:` or `file:` is added to
`$HOME/.apps/os.lazy.lazyweb/history.tsv` (one `time<TAB>url<TAB>title` line
each, the newest 1000 kept). `about:history` lists it, newest first, and can
clear it. LazyWeb's own pages (`about:history`, `about:downloads`,
`about:lazyweb` and the start page) are built in the browser; their links
`x-lazyweb:clear-history`, `x-lazyweb:open/N` and `x-lazyweb:cancel/N` act
only while one of those pages is on show, so a web page cannot use them.

A response the view cannot show, or one sent as an attachment
(`Content-Disposition: attachment`), is downloaded: saved without a prompt to
`$HOME/Downloads` (`/tmp` when there is no home), first as `<name>.part`,
renamed when complete, with ` (1)`, ` (2)` and so on added to a name already
taken. The status bar shows the progress and `about:downloads` lists every
download of the session, with a link to open a finished one and to cancel a
running one. File > Save Page As Download saves the current page the same
way. Serial evidence: `WEB:DOWNLOAD:START:<name>`,
`WEB:DOWNLOAD:DONE:<name>:<bytes>`, `WEB:DOWNLOAD:FAIL:<reason>`.

## Links and the rest of the OS

LazyWeb's package registers it with `mimed` for `x-scheme-handler/http`,
`x-scheme-handler/https` and `text/html`: `mimed` guesses
`x-scheme-handler/<scheme>` for any URL, and `init` passes a URL to the app
it launches (a launch argument is an absolute path or a URL). So
`sys::mimed::open("https://...", "open")` from a `rhai` script, a link in Mail, or a `.html` file in Files
opens LazyWeb. The other way round, a link LazyWeb cannot follow itself
(`mailto:` and any scheme the view does not fetch) is handed to `mimed`
(`WEB:LAUNCH:<url>:OK|FAIL`). Only a click, a key or a typed address may
do that (`WEB:LAUNCH:<url>:BLOCKED` is the refusal otherwise): Blitz has no
scripts and no meta refresh, so every launch it reports is the user's. Mail registers `x-scheme-handler/mailto` and
opens its compose window with the address, subject and body of the link.

## Fonts

Pages are drawn with the Liberation fonts (Sans, Serif and Mono with their
bold, italic and bold italic faces, SIL OFL 1.1) that desktop images install
in `/system/share/fonts/liberation`; LazyWeb reads them at start
(`xui-app/web/src/fonts.rs`, serial `WEB:FONTS:<loaded>/12`). They are
metric-compatible with Arial, Times New Roman and Courier New, so pages set
in those keep their line breaks, and italic text gets a real italic face.
CSS `sans-serif`, `serif` and `monospace` (and the families sorted into
them, such as Arial, Times or Courier) map to the three. The window itself
stays in Droid Sans, like every xui app.

## Cookies

Blitz keeps no cookies, so the fetcher does (`xui-app/web/src/fetch/cookies.rs`,
RFC 6265 without scripts): the `Set-Cookie` of every response, a redirect's
included, goes into an in-memory jar (a session: nothing is written to disk)
and matching cookies ride on later requests (`Domain`, `Path`, `Secure`,
`Max-Age` and `Expires` are honoured; at most 3000 cookies and 50 per host).
A `Domain` naming a public suffix (`com`, `co.uk`, `github.io`, by the embedded
public suffix list, the `psl` crate) is refused, unless it is the host itself,
which then keeps the cookie to itself. There is no `SameSite` handling: the fetcher cannot tell a picture on a page
from the page, so a third-party image carries its own site's cookies.

## Rendering notes

Each view has its own engine thread and every frame re-rasterises the visible
viewport on the CPU, on scroll and hover too: watch `WEB:TIME`. Scrollbars are
Blitz's overlay ones. The body's background image stops at the end of the body
instead of filling the viewport (upstream). A failed page shows xui-blitz's
error page, titled "Problem loading page", after `WEB:FAIL`.

## Wikipedia

Wikipedia and its sister projects serve the Vector 2022 skin, a CSS grid with
custom properties. NetSurf had neither, so LazyWeb used to ask the Wikimedia
wikis for their 2010 skin (`useskin=vector`, a rewrite in `sites.rs`). Blitz
lays Vector 2022 out (issue #632), so the rewrite is gone and pages are fetched
as linked; the harness's copies (`tools/web/wikicapture.py`) are Vector 2022
too. Known flaws: a floated infobox can run past the section rule under it,
and some inline text loses its spacing (Hacker News' header, "Firstappeared"
in infoboxes).

## Testing it

```bash
python tools/web/run.py                  # build, browse example.com and theoldnet.com, judge
python tools/web/run.py --precheck-only  # the harness alone: console image, curl checks
python tools/web/run.py --live           # the real sites, on a network that reaches them
python tools/web/test_judge.py           # the judge's own tests, the fixtures are current
```

The harness serves stand-ins for both sites from the host on their real ports
(a copy of example.com over HTTP, and a 90s-style theoldnet.com home page
over HTTPS whose certificate comes from a throwaway CA the test image trusts),
points the names at the host through the image's `/etc/hosts`, starts the
browser at `http://example.com/`, types `https://theoldnet.com/` into the
address bar, opens copies of two Wikipedia pages with their style sheets and
pictures (`tools/web/wiki.py`), downloads a file the site sends as an attachment, follows a
`mailto:` address, opens `about:history` and `about:downloads`, and judges
the browser's serial markers (`WEB:UP:PASS`,
`WEB:LOAD:<url>`, `WEB:TITLE:<title>`, never `WEB:FAIL:<reason>`), the requests
the host's servers saw (Host headers, SNI, every picture: PNG, JPEG, GIF) and
the screenshots. Details: [tools/web/README.md](../tools/web/README.md).

### Timing a load

The browser prints a timing line for every fetch and every navigation on its
standard output (the harness sends it to the serial console):

```text
WEB:FETCH:538ms 200 total=754 dns=44 tcp=44 tls=182 wait=408 body=73 347868B https://github.com/va1erian/lazyos
WEB:TIME:1995ms:nav     (and :done, :fail)
```

`WEB:FETCH` starts with when the fetch began, in milliseconds since the
browser started. Then come its status (`FAIL` when there was no response) and
the time of each step in milliseconds: the name lookup, the TCP connect, the
TLS handshake, the wait for the response headers and the body. A step that
did not happen shows `-`: `tls` on `http:`, or `dns` after a failed lookup.
Successful name lookups are cached by `host:port` for up to a minute, so later
fetches to the same `host:port` show `dns=0`; failed lookups are not cached.
The URL is shown without its user name, password or query values. `WEB:TIME` marks when a navigation started, finished or failed, on
the same clock.
