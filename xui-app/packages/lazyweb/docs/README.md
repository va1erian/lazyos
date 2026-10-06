# LazyWeb

LazyWeb is LazyOS's web browser, built on the NetSurf engine: HTML 4 and CSS
2.1 with parts of CSS 3, PNG, JPEG and GIF images (animated GIFs too), over
`http://` and `https://`. HTTPS certificates are checked against the system's
trust store (`/etc/ssl/certs/ca-certificates.crt`).

* Type an address in the address bar (**Ctrl+L** jumps to it) and press
  **Enter**.
* Click a link to follow it. The status bar shows where a link goes, and a
  padlock says whether the page came over HTTPS.
* The throbber in the toolbar spins while a page loads; **Esc** or the Stop
  button stops it.
* **Ctrl+H** shows your history, **Ctrl+J** your downloads. Files the browser
  cannot show are saved to the *Downloads* folder in your home.
* Links to other programs, such as `mailto:` addresses, open in the app that
  handles them.

LazyWeb needs the network: start LazyOS with `python tools/run_demo.py
--lazyweb` (or tick *LazyWeb browser* in the launcher).

LazyWeb is free software under the GNU General Public License, version 2 only,
like NetSurf, whose code it contains. See `docs/lazyweb.md` for the source
and the licence.
