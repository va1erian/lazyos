# LazyWeb

LazyWeb is LazyOS's web browser, built on the NetSurf engine: HTML 4 and CSS
2.1 with parts of CSS 3, PNG, JPEG and GIF images (animated GIFs too), over
`http://` and `https://`. HTTPS certificates are checked against the system's
trust store (`/etc/ssl/certs/ca-certificates.crt`).

* Type an address in the address bar (**Ctrl+L** jumps to it) and press
  **Enter**.
* Click a link to follow it.

LazyWeb needs the network: start LazyOS with `python tools/run_demo.py
--lazyweb` (or tick *LazyWeb browser* in the launcher).

LazyWeb is free software under the GNU General Public License, version 2 only,
like NetSurf, whose code it contains. See `docs/lazyweb.md` for the source
and the licence.
