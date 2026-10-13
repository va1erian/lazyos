# LazyWeb+

LazyWeb+ is LazyWeb's browser window on the WebKit engine, the engine of Safari, ported to LazyOS:
JavaScript, forms, modern HTML and CSS, pictures, and HTTPS with the system's certificate bundle.
LazyWeb (without scripts, on the Blitz engine) stays the browser LazyOS ships; this one is for the
pages that need a real engine. It is large: three programs of about 108 MB each (the window and
WebKit's UI process, a WebProcess that runs the pages, a NetworkProcess), each read whole into RAM
when it starts, so the first page takes a few seconds and the machine wants 4 GiB (`python
tools/run_demo.py --lazywebplus` sets the big-app profile, a larger user memory quota and a 1 GiB
OS volume). One WebProcess serves every page.

## Using it

* **Address:** `Ctrl+L`, type, `Enter`. A bare name such as `example.com` opens
  `http://example.com/`. `Esc` in the field puts the page's address back.
* **Menus:** File (Open Location, Save Page As Download, Close Window), View (Reload, Stop),
  History (Back, Forward, Home, Show All History, Clear History), Downloads, Help.
* **Keys:** `Alt+Left` and `Alt+Right` Back and Forward, `Alt+Home` Home, `F5` or `Ctrl+R` Reload,
  `Esc` Stop while a page loads (otherwise the page's), `Ctrl+H` history, `Ctrl+J` downloads,
  `Ctrl+S` save the page, `Ctrl+W` close.
* **Status bar:** the link under the pointer or what the browser is doing, the running downloads
  with a progress bar, and a padlock: closed over HTTPS, open over plain HTTP. A certificate that
  does not verify is an error, never a page you are asked to accept.
* **The pointer** follows the page: a hand over links, an I-beam over text, resize arrows.
* **JavaScript dialogs** (`alert`, `confirm`, `prompt`) are drawn over the window.
* **A page's new window** (`target=_blank`, `window.open`) opens in the same view. There are no
  tabs yet.

## History, cookies and downloads

The pages you visit are kept in `$HOME/.apps/os.lazy.lazywebplus/history.tsv`; `about:history`
lists them and can clear them. Cookies and site storage live in the same folder and survive
closing the window. A response the page cannot show, or one sent as an attachment, is saved without
a prompt to `$HOME/Downloads` (first as `<name>.part`); `about:downloads` lists them.

A link the browser cannot follow itself (`mailto:`, other schemes) goes to the app registered for
it through `mimed`. A page's script can never start another program: only your click or key can.
LazyWeb and LazyWeb+ both register `http`, `https` and HTML files; `user/<uid>/mimed/default/<type>`
(Settings, Default apps) says which one opens them.

## Notifications

A page can ask to show notifications (a mail site, a chat). When it asks, a bar appears above the page
with the site's name and **Allow**, **Block** and **Not now**; nothing shows until you choose. What you
choose is remembered per site and listed in `about:lazyweb`, where **Forget** takes a decision back. An
allowed site's notifications are toasts in the corner of the screen under the name LazyWeb+, with the
site's host as the title and the page's own words below it; a site can only post a few at a time. Click a
toast and LazyWeb+ comes forward (a site that has gone is opened again). LazyWeb+ itself, a download and
the system can never be impersonated this way: the title is always the site, never what the page says.

## Sandboxed

The two processes that do the work run in sandboxes of their own, and LazyOS enforces them, not WebKit:

* **The page process** (the one that runs a page's JavaScript with its JIT) can read the font files and
  nothing else. It has no network, no access to your files, no window or input device, no way to start a
  program and no way to signal another process. If a page exploits the engine, that is all it gets.
* **The network process** can use the network, read the system's resolver and certificate list, write
  inside the browser's own folder (`.apps/os.lazy.lazywebplus` in your home folder: cookies, site
  storage) and in your Downloads folder (a download is saved there first as `<name>.part`). It has no
  window, no input and no other file.
* **This window** is an ordinary app: it draws, forwards your keys and clicks, keeps the history and
  starts the two.

The sandbox is not a reason to open anything: a bug in this window or in the messages the page process
sends it is still a way out, and the page process runs as you. LazyWeb, which has no script engine, stays
the browser LazyOS ships.

## Licence

The window is under the GPL 2.0 or 3.0 (or later) as you choose; WebKit is LGPL-2.1 (with
BSD-licensed parts); the program links OpenSSL 3.5 (Apache-2.0, so the combination is GPL-3.0 or
later), FreeType, HarfBuzz, ICU, curl, SQLite and others, listed in `THIRD_PARTY.md`; the pages use
the Liberation fonts (SIL OFL 1.1). Whoever distributes an image must also offer the corresponding
source of WebKit (the revision the build pins).
