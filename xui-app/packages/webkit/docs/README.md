# WebKit

WebKit is the web engine of Safari, ported to LazyOS (WebKit2: a UI process, a WebProcess and a
NetworkProcess). This app is its window: a toolbar with Back, Forward and Reload, an address field,
the page, and a status line.

LazyWeb is the browser LazyOS ships; this one is the long road beside it, for pages that need a full
engine. It is large: three programs of about 108 MB each (the window and WebKit's UI process, a
WebProcess that runs the pages, a NetworkProcess), each read whole into RAM when it starts, so the
first page takes a few seconds and the machine wants 4 GiB (`python tools/run_demo.py --webkit` sets
the big-app profile, a larger user memory quota and a 1 GiB OS volume). One WebProcess serves every
page.

## Using it

* **Address:** type in the field and press Enter. A bare name such as `example.com` opens
  `http://example.com/`. `Ctrl+L` goes to the address field; `Esc` there puts the page's address back.
* **Back, Forward, Reload:** the toolbar buttons, `Alt+Left`, `Alt+Right`, `F5` and `Ctrl+R`. Reload
  turns into Stop while a page is loading.
* **The page:** click links, type into forms, scroll with the wheel. The title is in the window's
  caption; the status line shows the loading progress and the address.
* Pages run JavaScript (JavaScriptCore with the JIT) and show PNG, JPEG and GIF pictures (WebP is built in, not tried).
  Video, WebGL, WebAssembly, downloads, printing and the developer tools are not there yet.

## Sandboxed

The two processes that do the work run in sandboxes of their own, and LazyOS enforces them, not WebKit:

* **The page process** (one for every page, the one that runs a page's JavaScript with its JIT) can read
  the font files and nothing else. It has no network, no access to your files, no window or input device,
  no way to start a program and no way to signal another process. If a page exploits the engine, that is
  all it gets.
* **The network process** can use the network, read the system's resolver and certificate list, and write
  inside the browser's own folder, `.apps/os.lazy.webkit` in your home folder. It has no window, no input
  and no other file.
* **This window** is an ordinary app: it draws, forwards your keys and clicks, and starts the two.

The browser keeps its cookies, cache and site storage in that folder (`cookies/`, `cache/`, `storage/`).
Delete the folder to forget them; no history is kept yet. Nothing is shared with other apps.

The sandbox is not a reason to open anything: a bug in this window or in the messages the page process
sends it is still a way out, and the page process runs as you. LazyWeb, which has no script engine,
stays the browser LazyOS ships.

## What is where

* The app is `os.lazy.webkit`; its processes are `/system/bin/webprocess` and
  `/system/bin/networkprocess`, each with the label `app:os.lazy.webkit/<name>` that the `[processes.*]`
  tables of the package's manifest describe. The window talks to WebKit through a small C ABI
  (`webkit/port/app/lazyos_ui.h`).
* Your profile folder is `$HOME/.apps/os.lazy.webkit` (mode `0700`); without a home folder the browser
  keeps nothing.
* The plan, the port and the numbers: `docs/webkit-port-plan.md` (W4 and W5), the kernel's side of the
  sandbox in `docs/architecture/process-sandbox.md`, and `webkit/README.md` in the source tree.

## Licence

WebKit is LGPL-2.1 (with BSD-licensed parts); the window is GPL-3.0-or-later; the build links OpenSSL
3.5 (Apache-2.0) and FreeType, HarfBuzz, ICU, curl, SQLite and others, listed in `THIRD_PARTY.md`.
