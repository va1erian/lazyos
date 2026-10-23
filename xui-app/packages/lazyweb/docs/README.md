# LazyWeb

LazyWeb is the web browser of LazyOS, on the WebKit engine, the engine of Safari, ported to LazyOS:
JavaScript, forms, modern HTML and CSS, pictures, and HTTPS with the system's certificate bundle.
The window is a small app; the engine is **webd**, the web engine service of your session, which LazyWeb
asks for its page and which draws it for the window. webd and the processes it starts for the page and the
network are large programs, each read whole into RAM when it starts, so the first page takes a few seconds and
the machine wants 4 GiB (`python tools/run_demo.py --lazyweb` sets the big-app profile, a larger user
memory quota and a 1 GiB OS volume). One web process serves every page of the browser. If webd is not running
the window says so and tries again every second.

## Using it

* **Address:** `Ctrl+L`, type, `Enter`. A bare name such as `example.com` opens
  `http://example.com/`. `Esc` in the field puts the page's address back.
* **Menus:** File (Open Location, Save Page As Download, Keep Page Data in Memory, Close Window), View (Reload,
  Stop), History (Back, Forward, Home, Show All History, Clear History), Downloads, Help.
* **Keys:** `Alt+Left` and `Alt+Right` Back and Forward, `Alt+Home` Home, `F5` or `Ctrl+R` Reload,
  `Esc` Stop while a page loads (otherwise the page's), `Ctrl+H` history, `Ctrl+J` downloads,
  `Ctrl+S` save the page, `Ctrl+W` close.
* **Status bar:** the link under the pointer or what the browser is doing, the running downloads
  with a progress bar, and a padlock: closed over HTTPS, open over plain HTTP. A certificate that
  does not verify is an error, never a page you are asked to accept.
* **The pointer** follows the page: a hand over links, an I-beam over text, resize arrows.
* **JavaScript dialogs** (`alert`, `confirm`, `prompt`) are drawn over the window.
* **A page that stops answering** gets a bar with **Wait**, **Reload** and **Stop**.
* **A page's new window** (`target=_blank`, `window.open`) opens in the same view. There are no
  tabs yet, and local files (`file:`) do not open.

## What is kept, and where

* **Cookies, the cache and site data** are webd's: the browsing profile in
  `$HOME/.apps/os.lazy.webd/profiles/browser`. They survive closing the window. If you used an earlier
  LazyWeb, whose engine kept them in `$HOME/.apps/os.lazy.lazyweb`, they are **not carried over**: sites
  ask you to sign in again.
* **History, notification choices and the ad blocker's lists** are LazyWeb's own, in
  `$HOME/.apps/os.lazy.lazyweb`, and survive the change: the pages you visit are in `history.tsv`;
  `about:history` lists them and can clear them.
* **Downloads:** a response the page cannot show, or one sent as an attachment, is saved without a prompt to
  `$HOME/Downloads` (first as `<name>.part`); `about:downloads` lists them.
* **File > Keep Page Data in Memory** keeps cookies and site data in memory only, for a `/home` on a USB
  stick. In that mode the ad blocker is off and pages cannot show notifications.

A link the browser cannot follow itself (`mailto:`, other schemes) goes to the app registered for
it through `mimed`. A page's script can never start another program: only your click or key can.
LazyWeb registers `http`, `https` and HTML files with `mimed`; `user/<uid>/mimed/default/<type>`
(Settings, Default apps) says which browser opens them when there is more than one.

## Notifications

A page can ask to show notifications (a mail site, a chat). When it asks, a bar appears above the page
with the site's name and **Allow**, **Block** and **Not now**; nothing shows until you choose. What you
choose is remembered per site and listed in `about:lazyweb`, where **Forget** takes a decision back. An
allowed site's notifications are toasts in the corner of the screen under the name LazyWeb, with the
site's host as the title and the page's own words below it; a site can only post a few at a time. Click a
toast and LazyWeb comes forward (a site that has gone is opened again). LazyWeb itself, a download and
the system can never be impersonated this way: the title is always the site, never what the page says.

## Ad blocker

LazyWeb downloads a filter list (EasyList) and keeps the requests it names from being made. The shield in
the toolbar opens `about:blocker`: switch it off for a site or for everything, update the list, or reload
once without blocking. The list is downloaded by a separate process that may reach the network and write only the
lists folder of LazyWeb's folder.

## Sandboxed

The two processes that do the work for the page run in sandboxes of their own, and LazyOS enforces them, not
WebKit. They belong to webd:

* **The page process** (the one that runs a page's JavaScript with its JIT) can read the font files and
  nothing else. It has no network, no access to your files, no window or input device, no way to start a
  program and no way to signal another process. If a page exploits the engine, that is all it gets.
* **The network process** can use the network, read the system's resolver and certificate list, write
  inside webd's own folder (`.apps/os.lazy.webd` in your home folder: the browsing profile) and in your
  Downloads folder (a download is saved there first as `<name>.part`). It has no window, no input and no other
  file. It can read the cookies of every program that browses through webd.
* **This window** is an ordinary app: it draws, forwards your keys and clicks, keeps the history and
  the notification choices, and starts only the list download.

The sandbox is not a reason to open anything: a bug in this window or in the messages the page process
sends webd is still a way out, and the page process runs as you.

## Licence

The window is under the GPL 2.0 or 3.0 (or later) as you choose, and the program is distributed under the
GPL 3.0 or later (it links two crates of LazyOS's own under that licence). The engine, WebKit, is LGPL-2.1
(with BSD-licensed parts), and is in webd, which also links OpenSSL 3.5 (Apache-2.0), FreeType, HarfBuzz, ICU,
curl, SQLite and others, listed in `THIRD_PARTY.md`; the pages use the Liberation fonts (SIL OFL 1.1). Whoever
distributes an image must also offer the corresponding source of WebKit (the revision the build pins).
