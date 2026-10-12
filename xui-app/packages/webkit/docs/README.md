# WebKit

WebKit is the web engine of Safari, ported to LazyOS (WebKit2: a UI process, a WebProcess and a
NetworkProcess). This app is its window: a toolbar with Back, Forward and Reload, an address field,
the page, and a status line.

LazyWeb is the browser LazyOS ships; this one is the long road beside it, for pages that need a full
engine. It is large (three programs of about 108 MB each, started on first use) and loads each page in
a separate process, so give the machine a few GiB of RAM (`python tools/run_demo.py --webkit` sets
the big-app profile).

## Using it

* **Address:** type in the field and press Enter. A bare name such as `example.com` opens
  `http://example.com/`. `Ctrl+L` goes to the address field; `Esc` there puts the page's address back.
* **Back, Forward, Reload:** the toolbar buttons, `Alt+Left`, `Alt+Right`, `F5` and `Ctrl+R`. Reload
  turns into Stop while a page is loading.
* **The page:** click links, type into forms, scroll with the wheel. The title is in the window's
  caption; the status line shows the loading progress and the address.
* Pages run JavaScript (JavaScriptCore with the JIT) and show PNG, JPEG, GIF and WebP pictures.
  Video, WebGL, WebAssembly, downloads, printing and the developer tools are not there yet.

## What is where

* The app is `org.lazy.webkit`; its processes are `/system/bin/webprocess` and
  `/system/bin/networkprocess`. The window talks to WebKit through a small C ABI
  (`webkit/port/app/lazyos_ui.h`).
* WebKit keeps its cookies and storage under `~/.apps/org.lazy.webkit`.
* The plan, the port and the numbers: `docs/webkit-port-plan.md` (W4 and W5) and `webkit/README.md` in
  the source tree.

## Licence

WebKit is LGPL-2.1 (with BSD-licensed parts); the window is GPL-3.0-or-later; the build links OpenSSL
3.5 (Apache-2.0) and FreeType, HarfBuzz, ICU, curl, SQLite and others, listed in `THIRD_PARTY.md`.
