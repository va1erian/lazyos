# LazyWeb, the web browser

LazyWeb (`os.lazy.lazyweb`) is LazyOS's web browser: the
[NetSurf](https://www.netsurf-browser.org/) engine (HTML 4 and CSS 2.1 with
parts of CSS 3, PNG, JPEG, GIF and animated GIF, BMP and ICO through its
libraries) in a desktop window of the `xuid` compositor, fetching pages over
`http://` and `https://`. HTTPS uses the TLS work of
[tls-plan.md](tls-plan.md): rustls with the pure-Rust `nettls-crypto`
provider, certificates checked against the system bundle at
`/etc/ssl/certs/ca-certificates.crt`, names resolved by musl through
`/etc/hosts` and `/etc/resolv.conf`. TLS runs in the browser's own process; no
service sees its traffic.

Where it lives:

| Piece | Where |
|---|---|
| The app (crate `lazyweb`) | `xui-app/web`, a static musl xui client; NetSurf's C libraries compiled with zig by `tools/xui/build.py` into `target/xui/xui-lazyweb.elf` |
| The core package | `xui-app/packages/lazyweb` (manifest, icons, help), packed by `tools/xui/core_packages.py`; `pkgd` installs it into `/apps` at boot |
| The image switch | `LAZYOS_LAZYWEB=1` (`build_support/lazyweb_embed.rs`), which needs `LAZYOS_DESKTOP=1` and `LAZYOS_NETD=1` and fails the build, saying what to run, when the browser is not built |
| The front ends | `python tools/run_demo.py --lazyweb`; the launcher's *LazyWeb browser* (Simple tab, Network) and the raw switch (Advanced tab, Networking) |
| The harness | `tools/web/run.py` ([tools/web/README.md](../tools/web/README.md)) |

## Licence

NetSurf is licensed GPL-2.0-only, so LazyWeb, which links it, is
**GPL-2.0-only** too (`license = "GPL-2.0-only"` in its `Cargo.toml`), and
everything linked into the browser binary must be available under a
GPLv2-compatible licence: MIT, BSD, ISC, Zlib, Apache-2.0 *only alongside*
one of those (Apache-2.0 alone is not GPLv2-compatible), or the GPLv2 itself.
That is why the TLS stack avoids `ring` and `aws-lc` (§3.2 of
[tls-plan.md](tls-plan.md)), and why GPL-3.0 code cannot be linked: LazyOS's
own crates are GPL-3.0-or-later, so any that the browser links must be
offered under a GPLv2-compatible licence as well. `python
tools/nettls/licenses.py` is the gate for the TLS crates; the browser's own
dependency tree needs the same check. The package's help page states the
licence; whoever distributes an image must also offer the corresponding source
of NetSurf, its libraries and LazyWeb (the revisions the build pins).

## Running it

```bash
python tools/run_demo.py --lazyweb     # builds the browser (needs zig) and the desktop, boots it
```

`--lazyweb` implies `--desktop`, `--net` (a virtio-net card on QEMU's user
network, which reaches the internet through the host) and `--tls` (`curl`,
`wget` and `fetch` beside the browser). NetSurf is C, so building it needs zig:
`pip install ziglang==0.16.0`, then `python tools/xui/build.py`; `run_demo.py`
runs that when `target/xui/xui-lazyweb.elf` is missing. Open LazyWeb from the
desktop's menu (Settings -> Menu offers it, like every core package), or from
the Terminal:

```sh
$(echo /apps/os.lazy.lazyweb/*/bin/lazyweb.elf) --client http://example.com/ &
```

By hand: `LAZYOS_DESKTOP=1 LAZYOS_NETD=1 LAZYOS_NETD_ARGS=demo=0 LAZYOS_TLS=1
LAZYOS_LAZYWEB=1 cargo build`.

The package's permissions (display, input, `netd`'s stack interface and
outbound sockets) are modelled on Net Tools and still to be derived from a run
under its label: build with `LAZYOS_LABEL_TRACE=1`, browse, and read the
`LABEL:DENY` lines ([packages.md](packages.md)).

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
address bar, and judges the browser's serial markers (`WEB:UP:PASS`,
`WEB:LOAD:<url>`, `WEB:TITLE:<title>`, never `WEB:FAIL:<reason>`), the requests
the host's servers saw (Host headers, SNI, every picture: PNG, JPEG, GIF) and
the screenshots. Details: [tools/web/README.md](../tools/web/README.md).
