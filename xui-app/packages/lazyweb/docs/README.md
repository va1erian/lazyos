# LazyWeb

LazyWeb is LazyOS's web browser, built on the Blitz engine: modern HTML and
CSS (grid, flexbox, custom properties), PNG, JPEG, GIF, WebP and SVG images,
no JavaScript, over
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

LazyWeb is free software, declared under the GNU General Public License,
version 2 only; Blitz is MIT and Apache 2.0 (Stylo: MPL 2.0) and the Liberation
fonts are SIL OFL 1.1. See `docs/lazyweb.md` for the source and the licences.
