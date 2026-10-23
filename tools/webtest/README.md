# Shared web test sites

The stand-in sites and certificates the browser harnesses browse: LazyWeb's
(`tools/lazyweb`, [docs/lazyweb.md](../../docs/lazyweb.md)) and, for
the picture encoders, LazySky's mock server (`tools/lazysky`). There is no
runner here: each harness builds its own image, serves these sites from the
host and judges its own evidence. This directory was `tools/web`, the harness
of the LazyWeb browser on Blitz, which is gone.

```bash
python tools/webtest/test_sites.py          # the sites answer as the real ones do; fixtures are current
python tools/webtest/gen_fixtures.py [--check]  # redraw the test site's pictures
python tools/webtest/sites.py OUTDIR        # serve by hand (ports 80 and 443)
```

| File | |
|---|---|
| `sites.py` | the host's stand-ins, with a record of every request (scheme, Host, path, User-Agent, TLS SNI) that the judges read |
| `certs.py` | the run's throwaway CA and leaf, and the hosts file mapping the stand-ins' names to the host |
| `pages.py` | what the fixtures' home pages are called and which same-site files they reference |
| `fixtures/` | `example.com/` and `theoldnet.com/` (home page, pictures, style sheet, the `webkit/` and `lazyweb/` test pages) |
| `gen_fixtures.py`, `imgenc.py`, `jpegenc.py` | the pictures, drawn with the standard library only and checked in |

## The stand-in sites

The sandbox and CI cannot reach example.com or theoldnet.com, and the live
theoldnet.com shows only a maintenance notice, so the host serves both
(`sites.py`) and the image is told they live on the host:

| URL | Served from | |
|---|---|---|
| `http://example.com/` | `127.0.0.1:80` | `fixtures/example.com/index.html` |
| `http://theoldnet.com/...` | `127.0.0.1:80` | `301` to `https://theoldnet.com/...`, like the real site |
| `https://theoldnet.com/`, `https://www.theoldnet.com/` | `127.0.0.1:443` | `fixtures/theoldnet.com/`: a late-90s home page (table layout, `<font>`, `<center>`, a tiled `background=` GIF, a PNG logo with alpha, a JPEG photo, an animated GIF, a rainbow rule, a CSS file, links), the pages under `webkit/` and `lazyweb/`, a download, a cookie page, a slow page and an echo |

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
`sudo sysctl net.ipv4.ip_unprivileged_port_start=80`. The harnesses refuse to
start when something else holds either port, and two harness runs at once
(two agents) corrupt each other: check before a run.

The pictures are drawn by `gen_fixtures.py` with the standard library only
(`imgenc.py`: PNG and GIF with LZW and animation; `jpegenc.py`: a baseline
JPEG encoder) and checked in; `--check` (run by `test_sites.py`) keeps them
byte-identical to the generator.
