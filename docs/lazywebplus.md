# LazyWeb+, LazyWeb's browser on WebKit

`os.lazy.lazywebplus` ("LazyWeb+" in the menu) is LazyWeb's window, menus, history and downloads on the
WebKit engine of [`webkit-port-plan.md`](webkit-port-plan.md): JavaScript, forms, cookies, the modern web, in
place of Blitz. It is a second browser, not a replacement: LazyWeb (Blitz, no script engine, 12 MB of
memory, in every `LAZYOS_LAZYWEB=1` image) stays untouched and stays the browser an image ships by
default. Issue #835.

```bash
python tools/run_demo.py --lazywebplus    # desktop + networking + HTTPS + the browser (builds WebKit first: Docker)
python tools/webplus/run.py               # build, browse the stand-in sites twice, judge (tools/webplus/README.md)
cd xui-app && cargo test -p xui-webplus -p xui-webkit   # the window and the seam, offscreen, host
python tools/webplus/test_judge.py        # the judge fails when it should; the sessions are current
```

## What it is

One C++ program, the UI process of the W5 browser (`webkit/port/app/`), with LazyWeb's window as the Rust
static library it links (`xui-app/webplus`, crate `xui-webplus`): `lazywebplus-app`, 108 MiB stripped, which
starts `/system/bin/webprocess` and `/system/bin/networkprocess`. The package is
`xui-app/packages/lazywebplus` (core package, `LAZYOS_LAZYWEBPLUS=1`, which needs `LAZYOS_DESKTOP=1` and
`LAZYOS_NETD=1`; the OS volume defaults to 1 GiB). The W5 browser (`os.lazy.webkit`, `LAZYOS_WEBKIT=1`) is
the same engine with a bare window; both programs come from one C++ source, and
`lzw_ui_features()` tells the UI process which of the new behaviours the window it links wants (the W5
window asks for none, so it behaves as before).

The window is a fork of `xui-app/web`'s LazyWeb on the engine seam of `xui-app/crates/webkit` (`xui-webkit`:
the `Engine` trait, the mailbox between the two threads, the generic page view, the pure ABI helpers).
`xui-webkit` is built with its `window` feature for the W5 browser and without it for LazyWeb+, which
defines the per-window symbols the C++ side calls itself (`lzw_ui_main`, `lzw_ui_features`,
`lzw_ui_marker_prefix`, `lzw_ui_profile_dir`, `lzw_ui_navigate`, `lzw_ui_download_start`, ...).

LazyWeb's look is kept: the menu bar (File, View, History, Downloads, Help), the toolbar (Back, Forward,
Reload which turns into Stop, Home, the address field, an "open in the OS" button and the throbber), the
status bar (hover link, what the engine is doing, running downloads with a progress bar, the padlock) and the
same keys (Ctrl+L, F5, Alt+Left, Alt+Right, Alt+Home, Ctrl+H, Ctrl+J, Ctrl+S, Ctrl+W, Esc to stop while a page
loads: Enter and Esc are the address field's only while it has the focus, because a real engine has pages
that use them). The window title is `<page title> - LazyWeb+`.

## The ABI the engine needed

Everything from W5's sixteen functions is kept; the additions are what an engine with scripts needs and Blitz
did not (all in `webkit/port/app/lazyos_ui.h`, each mirrored in `xui-app/crates/webkit/src/{abi,ffi,events}.rs`
with host tests and in the recording engine; the table with the WebKit side is in
[`webkit/README.md`](../webkit/README.md)):

| Need | ABI |
|---|---|
| Features, marker prefix, profile directory | `lzw_ui_features`, `lzw_ui_marker_prefix`, `lzw_ui_profile_dir` |
| Committed URL, the padlock, how a load ended | `lzw_ui_state` grows `committed_url`, `secure`, `finished` |
| Link under the pointer, pointer shape | `lzw_ui_hover`, `lzw_ui_cursor` (patch `0016` teaches WebKit's PlayStation view the I-beam, wait and resize arrows) |
| Navigation policy | `lzw_ui_navigate(url, kind, user_gesture, committed_url)` |
| Downloads | `lzw_ui_download_start`, `_progress`, `_done`, `_fail`; `lzw_engine_download`, `lzw_engine_download_cancel` |
| `alert`, `confirm`, `prompt` | `lzw_ui_dialog` (returns at once), `lzw_engine_dialog_reply` |
| `window.open`, `target=_blank` | `lzw_ui_new_window` |
| Notifications API | `lzw_ui_notify_show`, `_cancel`, `_gone`, `_permission_request`, `_permission_gone`, `lzw_ui_notify_permission_at`; `lzw_engine_notify_shown`, `_clicked`, `_closed`, `_permission`, `_forget` (see *Notifications*) |
| A call on the process's first thread | `lzw_engine_run_on_main` |

## Navigation policy: a script never starts anything

LazyWeb could take it for granted that every launch it reports is the user's, because Blitz has no scripts.
WebKit cannot. Every navigation, redirect and window request goes through `decidePolicyForNavigationAction`
to `policy::decide(url, kind, user_gesture, from_own_page)` (`xui-app/webplus/src/policy.rs`, pure and
host-tested), with the gesture flag from `WKNavigationActionHasUnconsumedUserGesture` (a click or a key, not
a timer, a load event or a redirect):

| URL | Decision |
|---|---|
| `http:`, `https:`, `file:`, `data:`, `blob:`, `about:` | the engine loads it |
| `x-lazyweb:` (the browser's own commands) | runs only from one of the browser's own pages, for a click on a link there, and never goes to another app; anything else is refused |
| `mailto:`, `tel:`, any other scheme | handed to `mimed` only when a user gesture started it; otherwise refused and reported (`WEBPLUS:LAUNCH:<url>:BLOCKED`) |

**A second lock.** The engine's gesture flag is one boolean from a large engine, and a click may authorise more than
one thing (a timer chain, a redirect), so a hand-off to another program also needs real input the window itself
forwarded to the page within the last three seconds, and uses it up (`xui_webkit::gesture`, `lzw_ui_navigate`): one
click, one hand-off. A hand-off without it is refused and reported like a script's.

An address the user types is opened by the window itself (`handoff.rs`), so a navigation the UI process
started to `mailto:` with no gesture (kind `Other`) is a script's, a redirect's or a bug's, and is refused.
The browser's own pages are `data:` URLs built in the window; "on one of the browser's own pages" is an exact
comparison with the page the window built (`Internal::name_for`), so a web page that looks like
`about:history` and links to `x-lazyweb:clear-history` is not one. `window.open` and `target=_blank` load in
the one view, and only when a gesture started the request (`WEBPLUS:POPUP:<url>`); a script that opens a
window on its own gets nothing. The harness page `webplus/script.html` tries a `mailto:`, an
`x-lazyweb:clear-history` and a `window.open` with no click, and the judge requires all three to be refused.

## Pages

`about:history`, `about:downloads` and `about:lazyweb` and the start page are HTML built in the window and
shown as `data:` URLs (so there is one rendering path), reached by the menu, Ctrl+H and Ctrl+J. History is
`$HOME/.apps/os.lazy.lazywebplus/history.tsv` (the newest 1000 pages that finished loading over `http:`,
`https:` or `file:`), cleared from `about:history`.

## Downloads

A response the view cannot show, or one sent as an attachment, is downloaded: the response policy asks for a
download (`WKFramePolicyListenerDownload`), the window picks the destination (`Saver`: the name made safe,
unique among the files there and the downloads under way, ` (1)`, ` (2)`; the folder is made; nothing is
created in advance, because WebKit does not overwrite) and the NetworkProcess writes
`$HOME/Downloads/<name>.part`, renamed when the download completes and removed when it fails or is cancelled.
The size reported is the file's. The status bar shows the progress while it runs and `about:downloads` lists
the session's downloads with open and cancel links. File > Save Page As Download saves the current page the
same way. A download needs no gesture (an attachment is a response, not a click), so a page can start them: a
session is capped at 256 downloads, a name that is another download's `.part` is never reused, a file that
appeared at the final name meanwhile is never replaced (the download fails instead), invisible direction
characters are stripped from names, and with no home folder nothing is saved (the manifest lets the network
process write the Downloads folder of a home only). Each download also asks for a toast (`os.lazy.notify`);
see *Known limits*.

## Dialogs

`alert`, `confirm` and `prompt` arrive on the UI client's V6 callbacks with a result listener. The UI process
keeps the listener, tells the window (`lzw_ui_dialog`, which returns at once: the frame mailbox is never
blocked) and answers when the window calls `lzw_engine_dialog_reply`, or abandons it when the page goes away.
The window draws them with xui's `Dialog::message/confirm/prompt` ("theoldnet.com says"), modal to the page
area.

## Notifications

A page's `Notification` API works: `Notification.requestPermission()`, `new Notification(title, {body, tag})`,
and the `show`, `click` and `close` events. WebKit has the machinery in the UI process (the notification
manager, the provider, the UI client's permission request) but a port supplies the provider, or every page
sees `denied` and nothing shows; `webkit/port/app/notify.cpp` is that provider (feature
`LZW_FEATURE_NOTIFICATIONS`), and `ENABLE_NOTIFICATIONS` is on in the port's build (`tools/webkit/port_cmake.py`
passes it, because CMake keeps the first default in a build tree's cache). It turns every WebKit object file
over once; a build tree configured before it rebuilds. The shell's toasts (`os.lazy.notify`,
[`notify-plan.md`](notify-plan.md)) are the other half; the model between them is `xui-app/webplus/src/notify.rs`
and its parts, no window and no engine needed to test it.

* **Permission is per origin, asked once, kept.** A request starts a bar above the page (the browser's own
  chrome, not a toast: a toast never takes the keyboard, and an answer must never depend on one) naming the host
  with *Allow*, *Block* and *Not now*. Allow and Block are written to
  `$HOME/.apps/os.lazy.lazywebplus/notify.tsv` (`origin<TAB>allow|block<TAB>seconds`, the origin as WebKit
  writes it, `https://example.com` or `http://host:8080`) and told to WebKit
  (`WKNotificationManagerProviderDidUpdateNotificationPolicy`); the file is read back when WebKit starts a page
  process (`notificationPermissions`) and again at start-up. *Not now* refuses the pending requests and keeps
  nothing: the site may ask again. A request that came without a user gesture is refused outright (WebKit does
  it before the UI process hears of it; the window looks at the input it forwarded as well), and so is one for a
  page that is not `http:` or `https:` (`data:`, `about:`, `file:`). One site is asked at a time, four at most
  wait, and a bar goes when its page does. `about:lazyweb` lists the decisions with a *Forget* button each:
  the page's `Notification.permission` is `default` again and the site's live toasts go.
* **Identity.** The toast is posted by the UI process, so the shell shows it under **LazyWeb+**, and the title
  is the host (`example.com`, `host:8443`: never the page's words; a host too long for it keeps its end).
  The page's `title` and `body` go in the toast's body as two lines (the body may now hold a line feed, which
  ends a line), control, bidirectional-override and zero-width characters removed, cut to the shell's limits (64
  and 256 characters). The level is always Info, the icon the app's own (the page's `icon` is fetched by the
  page's own process, as WebCore does before it shows, and the toast does not use it),
  there is no action and the timeout is the level's, so nothing a page sends can look like a download or a
  system warning.
* **`tag` replaces in place**: the same tag from the same site updates the live toast (`replaces_id`), and the
  page's earlier object gets its `close`.
* **A click brings the page back.** The shell raises the app's windows on a click
  (`ActivateApp(label, session)`, shell-only, below), the window then tells WebKit
  (`WKNotificationManagerProviderDidClickNotification`) so the page's `click` handler runs, when its document is
  still there. A notification of a page that has gone (WebKit says so with `didDestroyNotification`; the toast
  stays) opens the page's address in the one view, when it is a page of the notifying site (otherwise the site's
  front page); the old handler cannot run, there is no document for it. A toast the person dismisses or that
  times out is a `close` event for the page.
* **Limits are per site** ([`notify/live.rs`](../xui-app/webplus/src/notify/live.rs)): 3 live toasts, a burst of
  3 posts and one more a second; 10 live web toasts in all, so the shell's 16 and its burst of 6 are never used
  up by a site, and a download's toast always has room. What a site posts beyond that is dropped
  (`WEBPLUS:NOTIFY:DROP:<origin>:<why>`), not queued. A toast outlives its page until its timeout; closing the
  browser takes its toasts down (the worker closes what it made).
* **Nothing persists but the permissions**: no notification history (the shell keeps none either).
  The file is in the folder the network process may write, like `history.tsv`: it is read only for
  canonical `http:`/`https:` origins and at most 512 lines, but a compromised network process can still
  write `https://evil.test<TAB>allow<TAB>0` and have a standing permission at the next start.
* **The page's process is not trusted to say whose a notification is.** A notification is shown only when
  its origin is the origin of the page the UI process reports on screen, and a request for the permission
  opens a bar only for that page's own origin; anything else is `DENY:<origin>:page`. A notification from a
  cross-origin frame is therefore refused. A notification over the limits is answered as closed, so the
  page's object does not stay "showing".

The shell calls are made on **WebKit's main thread** (`lzw_engine_run_on_main`): the notification service names
an app by the task id of the sender, and a message from a thread other than the process's first carries the
thread's own id and is refused as "not an app" (this is why the download toasts did not show before this
change). The shell's own half changed in three ways: it hands the click to the compositor
(`ActivateApp(label, session)` in `idl/display.midl`, from the label and session the kernel stamped on the
notification calls, so whichever thread made the window is covered), a toast body keeps its line feeds, and
the platform client (`xui_app::platform::notify`) can `Listen`.

Markers (UI-probe images only: they name sites), after `WEBPLUS:NOTIFY:`: `ASK:<origin>`,
`ALLOW|BLOCK|DISMISS:<origin>`, `SHOW:<origin>:<id>`, `CLICK:<id>`, `CLOSE:<id>`, `DROP:<origin>:<why>`,
`DENY:<origin>:<why>`, `FORGET:<origin>`. Verified by `python tools/webplus/notify_run.py`
([`tools/webplus/README.md`](../tools/webplus/README.md)) and the host tests of `xui-app/webplus`
(`tests/notifications.rs`, `notify/*`) and `lazyshell`.

## The profile

The data store is built by `createDataStore` in `webkit/port/app/engine.cpp` (#836): every directory of it
below one profile folder, which the window names (`lzw_ui_profile_dir`: `$HOME/.apps/os.lazy.lazywebplus`;
the W5 window gets `$HOME/.apps/os.lazy.webkit`). Cookies (`cookies/cookie.jar.db`), the HTTP cache and site
storage live there, so a second run of the app finds the first one's cookie and history (the harness checks
both, from the OS volume and from the second boot). The folder and `$HOME/Downloads` exist before a child that
may only write inside them looks for them.

**The HTTP cache is used across boots.** The second boot of the harness reloads theoldnet.com, which the first
one fetched with `max-age=604800`: it is served from the cache on the profile, no request for the page or any
of its pictures reaches the host (`judge_servers` fails if one does), and a task killed by a fault fails the
run. That read path once killed a
network-process thread: `NetworkCache::fileTimes()` had no branch for a port with neither `st_birthtime` nor GLib,
fell off its end, and clang compiled the whole cache-hit refresh to a trap (patch `0017`; `webkit/README.md`).
Without a creation time every record's "worth" is 0, so the cache's random shrink deletes each record with the
same probability (less for a shared body; it is not an LRU), and a hit always refreshes the modification time
(one `utimensat` per hit: the once-an-hour throttle needs a creation time that differs from it).

**The cookie jar needed a change to the SQLite build.** WebKit opens its databases with write-ahead logging,
whose index is a mapped `-shm` file (`unixShmMap`); LazyOS has no file-backed `mmap`, so every open failed
with `SQLITE_IOERR_SHMMAP` ("disk I/O error"), `CookieJarDB` gave up and *no cookie was ever stored*, not even
for the session (found by logging the open from the NetworkProcess). The bundle's SQLite is now built with
`SQLITE_OMIT_WAL` (`tools/webkit/recipes_text.py`): a rollback journal on plain `pread`/`pwrite` and `fcntl`
locks. Drop it when the kernel can map a file shared (PR #837).

## `mimed`: which browser opens a link

`mimed` keeps one app per `(type, verb)` and the last to register wins, so with LazyWeb and LazyWeb+ both
registering `x-scheme-handler/http`, `x-scheme-handler/https` and `text/html`, the one installed last would
open every link. There was no preference, so one was added: the confd key `user/<uid>/mimed/default/<type>`
(a string holding an app id; `libs/deskmenu/src/defaults.rs`), written by **Settings > Default apps** (all three
types at once) and honoured by `mimed` for the uid the kernel stamped on the `Open`, only while that app still
holds a registration for the type, so a stale choice (an uninstalled browser) falls back to the registry.
`confd` lets a uid touch only `user/<uid>/**`, so one account's choice never moves another's. Evidence:
`MIME:PREFER:PASS` in `mimed`'s selftest, host tests in `deskmenu`. With both browsers installed and no choice made, `mimed`
keeps its old rule (the app registered last wins, which is install order), so LazyWeb+ may open links until the
user picks LazyWeb in Settings; the lookup of the choice is made only when two apps handle the type.

## Blocking ads

LazyWeb+ blocks the requests the community filter lists mark as ads (issue #845), so a page loads less: a
blocked resource is never fetched, its script never runs and its box is never laid out. v1 ships **EasyList**,
network blocking only (no element hiding), fetched at runtime and never part of the image.

**The mechanism is WebKit's content extensions** (Safari content-blocker rule lists): JSON compiled to a bytecode
automaton by the UI process, stored on disk, shared with the web processes and enforced before a request leaves.
Four pieces make it work here:

* **The engine.** `ENABLE_CONTENT_EXTENSIONS` is on in `OptionsLazyOS.cmake` (WebKit has it off for this port by
  default). With it on the UI process did not link with the curl network backend; patch `0019` supplies
  `NetworkCache::Data::tryCreateSharedMemory()`. Patch `0018` makes the C API report a *blocked* load, so the
  count needs no second rule per block. `webkit/port/app/blocking.cpp` holds the store
  (`<profile>/contentextensions`), the page's user content controller and the per-navigation decision.
* **The converter**, `xui-app/crates/adblock` (`lazyadblock`, MIT, no dependencies): Adblock Plus syntax to
  rule lists. It emits only what WebKit's own parsers accept (one refused pattern fails a whole list, so it is
  strict and its tests parse every output with an independent parser of WebKit's URL-filter subset), splits at
  the 150,000-rule limit repeating the exceptions in each part (an exception only reaches back inside its own
  list), and drops what WebKit cannot say exactly, counting it, never widening it. `README.md` of the crate has
  the table of what becomes what and what is dropped.
* **The window's half**, `xui-app/webplus/src/blocker/`: the user's settings, the per-navigation decision, the
  lists and the worker that fetches and converts them, and `about:blocker`.
* **The fetch**, `curl` (the `nettls` program, `LAZYOS_TLS=1`: `run_demo.py --lazywebplus` and the launcher
  bring it), run by the window as a child under a label of its own, `app:os.lazy.lazywebplus/listupdate`
  (`[processes.listupdate]` in the manifest): outbound network, the resolver, the host table and the CA bundle,
  and write access to `$HOME/.apps/os.lazy.lazywebplus/lists`, nothing else. It sends no cookie and no referrer
  (curl keeps none), so a list server cannot tie a request to the browsing profile. A conditional request (the
  list's `ETag` and `Last-Modified`) is made at most when the list's own `! Expires:` line says so (bounded
  from one hour to fourteen days; EasyList says four days), at most once an hour after a failure, and on
  *Update now*.

### How a list becomes a block

1. The worker reads `lists/<id>.txt` and its record `lists/<id>.meta` (the source's hash, the engine's ids of
   the compiled parts, the ETag, when it was fetched and tried).
2. A source whose hash is the record's is **not converted**: the engine is asked for the parts the store holds
   (`lzw_engine_blocking_load`, which costs a millisecond or so for EasyList); only when the store has none (a
   new profile, a WebKit that changed its format) is the text converted and compiled
   (`lzw_engine_blocking_compile`, seconds on a fast host).
3. Parts are named `<list>-<hash of the source>-<n>`: a new version of a list is a new name.
4. A new version is **tried first**: its parts are compiled while the old ones keep blocking, and only when the
   engine has taken every part does it replace `lists/<id>.txt` and the record, and the old parts are forgotten.
   A version the engine refuses (one pattern its parser does not take fails a whole list) changes nothing: the
   old list keeps blocking, the refused version's validators are not kept, and it is tried again at the next
   due time (`WEBPLUS:BLOCK:REFUSED:<id>:...`).
5. A failed or broken download (not HTTP 200, no `[Adblock` header, too short) changes nothing either. A list
   nobody could fetch yet shows why on `about:blocker`. A list over 16 MiB, or one that becomes more than
   500,000 rules, is refused whole.
6. With the blocker switched off **nothing is fetched, loaded or compiled** (the engine's lists stay as they are
   and decide nothing); switching it on again does the work.
7. A page loads **unblocked until its list is ready**; nothing waits for it.

### Switching it off when a page breaks

* **This site**: *Turn off for <site>* on `about:blocker` (the toolbar's shield opens it) excepts the site and its
  subdomains for good. The engine takes the lists off the page for a main-frame navigation to such a site and
  puts them back for the next one (`lzw_ui_blocking` is asked before the load starts), so nothing is
  recompiled.
* **Once**: *Reload once without blocking* (also View > Reload Without Blocking) loads the page whole until the
  browser leaves its site; nothing is remembered.
* **Everything**: *Turn ad blocking off*, and a list can be switched off by itself.

The switches are `confd` keys under `user/<uid>/lazywebplus/blocker/` (`enabled`, `exceptions`, `lists_off`):
the user's, out of the engine's processes' reach (the network process may write the profile folder, and a
compromised one must not be able to switch the blocker off for a site). Without `confd` they last for the run.
The price is `os.lazy.confd.v1` in the app's permissions, which the UI process (the one a browser exploit
would have to escape into) holds for every `user/<uid>/**` key, as the other core apps do.
`x-lazyweb:` commands of `about:blocker` obey the rule of the other internal pages: only from that page and only
for a click.

### What it does not do

Element hiding (the part of a list that hides boxes: `##`, `#@#`, the `$elemhide` family; it is counted and not
used), `$redirect`, `$removeparam`, `$csp`, scriptlets (code injected into pages: not in v1), regexes WebKit's
parser cannot take (alternation, `{n,m}`, `\b`), non-ASCII patterns, and `$domain=` with both included and
excluded domains. A trailing `^` matches a separator character and not the end of the address, so such a filter
can only block *less* than Adblock Plus would. More lists (EasyPrivacy, Peter Lowe's, uBlock's), element hiding
and a user filter editor are separate issues.

### Results (issue #845)

* **The real list** (EasyList of 13 Oct 2026, 82,058 lines): 56,918 rules (56,331 blocks, 587 exceptions) in 12 MB
  of JSON, 0.11 s to convert in release on a fast host. Not used: 24,822 element-hiding lines, 18 lines with an
  option WebKit has no trigger for, 19 patterns outside WebKit's regex subset, 3 exceptions (which would cancel a
  block, so those blocks stay: `dropped_exceptions`), 8 duplicates.
* **Through WebKit itself** (`python tools/webkit/blockcheck.py --list ...`, the port's `webkit-smoke` in rules
  mode, 20-CPU Docker host): the list is accepted; `WKUserContentExtensionStoreCompile` 2.8 s, process peak
  resident set 391 MB; read back from the store in under 2 ms (what every later start pays); on a page with two
  EasyList-matching requests and one clean one, 2 loads blocked, 0 on a clean page.
* **In the guest** (`python tools/webplus/run.py --label-trace`, WHPX, 4 GiB; the harness's small list, four
  boots of one image): the `listupdate` child fetched the list under its own label (no `LABEL:DENY`; the nine
  documented optional denials of #836 are the only ones), it was converted, compiled by WebKit in the UI
  process, shared with the web processes (file-backed `mmap` and a passed descriptor) and attached; a later
  boot loaded it from the store with no conversion; a page loaded with the ad picture, the tracking pixel and
  the ad script **never requested** (the host saw each exactly once, after the user excepted the site) and its
  excepted script and own picture loaded; *Update now* sent the ETag and was told 304; the exception and the
  compiled list outlived a boot. All the browser's earlier checks (HTTPS pages, dialogs, downloads, cookies,
  the sandbox) pass with content extensions on.
* **The real list in the guest** (a fresh image whose list host serves the real EasyList, WHPX, 4 GiB): fetched
  (2,150,374 bytes), converted to 56,918 rules (24,859 filters not used) in about a second, compiled and attached
  by WebKit in about 4.3 s, ready 4.8 s after the download ended, with the browser up throughout (the compile is
  on the UI process's work queue); a later start loads it from the store. Memory of that compile in the guest and
  page-load times on real pages, with and without blocking, were **not** measured.
* **Found on the way**: the UI process did not link with content extensions on the curl backend (patch
  `0019`), and the guest harness's second visit of a page with a cached picture killed the NetworkProcess: the
  `NetworkCache::fileTimes()` crash that patch `0017` fixes (#847), unrelated to blocking.

## Permissions

The app: `os.lazy.display.v1`, `os.lazy.display.cursor.v1`, `os.lazy.input.v1`, `os.lazy.mimed.v1`,
`os.lazy.notify.v1`, `os.lazy.confd.v1` (the blocker's switches), outbound network,
`write:$HOME/.apps/os.lazy.lazywebplus` and `write:$HOME/Downloads`
and the reads the children need, derived from a `--label-trace` run of the harness. The ad blocker's
`listupdate` child (`curl`) holds the network, the resolver, the host table, the CA bundle and
`write:$HOME/.apps/os.lazy.lazywebplus/lists`, and no interface. The two engine processes
run **sandboxed** like W5's ([`architecture/process-sandbox.md`](architecture/process-sandbox.md), #836): the
manifest's `[processes.webprocess]` and `[processes.networkprocess]` tables give `/system/bin/webprocess` and
`/system/bin/networkprocess` the labels `app:os.lazy.lazywebplus/<name>`, which the kernel enforces for files,
sockets, process creation, signals and inherited descriptors. The page process reads fonts and nothing else;
the network process reads the resolver, the host table and the CA bundle, uses outbound sockets and writes the
profile folder and `$HOME/Downloads` (the `.part` file of a download is its to write; the window renames it).
Anything the UI process passes to the children is a socket. The UI process is an ordinary app: its file and
network entries are consent text, not enforced. A bug in the window, or in what the page process sends it, is
still a way out, and the children run as the user. LazyWeb stays the browser LazyOS ships.

## Licence

`xui-webplus` and `xui-webkit` are declared `GPL-2.0-only OR GPL-3.0-or-later`. LazyWeb is `GPL-2.0-only`
(the licence NetSurf required), but this program links OpenSSL (Apache-2.0, through WebKit's curl), which a
GPL-2.0-only program cannot carry; offered under GPL-3.0-or-later as well, the combined work is distributed as
GPL-3.0-or-later and the window crates stay usable in LazyWeb's own tree. `python tools/nettls/licenses.py
--lazywebplus` checks the window's Cargo tree for GPLv2 compatibility with one waiver, `unicode-linebreak`
(Apache-2.0 only, in every xui app through `xui-canvas`), and `python tools/webkit/licenses.py` checks the
WebKit bundle's rows and that the window crates keep both licences and name no OpenSSL crate. `THIRD_PARTY.md`
has the rows. Choosing GPL-3.0-or-later for the program is a decision for the maintainer to confirm.

## Known limits

* **Back to an older internal page.** The window remembers the newest `data:` URL of each of its own pages, so Back
  to an older History or Downloads page is no longer "its own": its buttons answer "Blocked: the page tried to open
  another app" (safe, but useless) until the page is opened again.
* **Dialogs** are not rate-limited (a page that loops `alert()` keeps the user behind modals) and are titled with
  the top-level site, not the frame that asked.
* **Evidence lines** are `println!`s (the `Webplus` trace channel exists and is not used yet; the ones that name a page,
  an address or a file print only in a `LAZYOS_UI_PROBE=1` image).
* **`history.tsv` is in the folder the network process may write** (the profile): the window reads only the last 2 MiB
  of it, cuts titles at 512 characters and drops addresses over 2048, but a compromised network process can still
  forge entries.
* **A page served from the HTTP cache crashes the network process on a later boot.** The notification harness
  found it: the second boot loads a page the first one cached and a thread of `NetworkProcess` jumps into
  padding in `NetworkCache::Storage::updateFileModificationTime`'s callable (`rip` at the `cc` filler before a
  function; `internal error (code 300)`, the load fails). It is not about notifications (the first boot's
  cache is what the second reads; the old harness's second boot only loaded a page that is never cached), and
  the harness serves its notification page `no-store` so it does not depend on it.
* **`Notification.close()` during a navigation leaves the toast up.** A page that enters the back/forward cache is
  suspended and WebKit closes its notifications exactly as `close()` does (the provider cannot tell them apart),
  so a close that arrives while a main-frame navigation is loading is taken as the suspension: the toast stays
  until its timeout, as the notification of a page that is gone. (Turning the page cache off would tell them
  apart, and was tried: Back then reloads from the HTTP cache, which hits the crash above.)
* **A startup crash, in about every second launch of one harness.** The UI process dies at its first resize
  (`memcpy` of a 1,800,000-byte frame past the end of its destination, a page fault at the first byte of a 2 MiB
  region, rip in `compiler_rt` `copyBlocks`) before `UP:PASS`. Seen once earlier and not reproduced, then in two
  of four launches on 2026-10-13, with the program the image shipped and with a rebuilt one. The destination is a
  block `mremap` had just moved. Suspected, not yet shown by a test: the moved copy-on-write pages create the
  region's page-table parents non-writable (the `x86_64` crate's `map_to` takes parent flags from the leaf), and
  `cow_fault` makes only the leaf writable. Issue #853; `lazywebplus-performance-plan.md`, section 8.
* **Toasts are made on WebKit's main thread.** The shell names an app by the task id of the sender and a
  thread other than the process's first carries its own id, so a toast from the window's thread or the download
  thread was refused as "not an app". The app now makes every call of the notification service from the first
  thread (`lzw_engine_run_on_main`), which costs the run loop one shell round trip (about 30 ms at most) per
  toast. The proper fix is a kernel stamp of the thread group's leader; this one needs none.
* The window and WebKit run on two threads of one process; the page area is drawn at device scale 1 (small
  text at 2x).
* No tabs, no find in page, no context menu, no pasteboard beyond WebKit's own, no IME, no WebGL, video or
  WebAssembly.
* **The ad blocker's lists, and the compiled rules, live in the profile folder**, which the network process may
  write: a compromised network process could replace a downloaded list (with a list that blocks everything,
  say), though not the user's switches (`confd`). The worse case is the compiled store (`contentextensions/`):
  WebKit maps the file and shares the mapping with the web processes without verifying it, so a compromised
  network process that writes it (or writes through the path to the shared pages) feeds bytecode to the
  interpreter of the web process, which is a way across the sandbox that does not exist with the store
  outside the network process's reach. The cure is to keep the lists and the store in a folder of their own
  (a sibling of the app's profile, which no process label but the app's may write; label rules are by path
  segment), and it needs a guest run to confirm what the kernel lets an app create there: left for a follow-up.
* **The list file is read whole**, bounded at 16 MiB and 500,000 rules, and converted on a thread of the
  window's process; WebKit compiles it in the UI process, so a hostile list that stays inside the bounds still
  costs seconds of one core and a few hundred MB while it compiles. Regular expressions that match the empty
  string (a rule that would block everything) are refused by the converter; nested quantifiers are not
  detected.
* **A host the site list cannot spell** (an IPv6 literal) is blocked like any other but cannot be excepted by
  name.
* **The filter lists need `/system/bin/curl`**, which `LAZYOS_TLS=1` ships (`run_demo.py --lazywebplus` sets
  it). A build with `LAZYOS_LAZYWEBPLUS=1` alone browses and blocks nothing, and `about:blocker` says "curl did
  not run".
* The blocked count of `about:blocker` and the shield is for the web page on show or browsed last (opening
  `about:blocker` does not reset it); a load a script makes in a loop is counted as often as it is blocked.
* The prompt dialog does not seed the page's default value.

## Paint counters

Once a second, while the page is painted, the UI process prints `INFO:PAINT:` (after the window's prefix):
`mode`, `n` (paints), `needs` (damage reports WebKit made), `px` (pixels painted), the microseconds summed over the
second in `clear`, `wk` (`WKPagePaint`), `swz` (conversion) and `post` (handing the frame to the window), `avg` and
`max` per paint, `lat` (the longest wait from a report to the paint that took it), `wheel` and `mouse` (calls from
the window), and `w2d` (average/longest microseconds from a wheel turn to the first report after it: the
WebProcess's share of a scroll step). Only the UI process's own time is in it; the window thread and the compositor
are not. In an image built with the UI probe, a file `<profile>/paint-mode` holding `legacy` (the whole view painted
and converted on every report, as before) or `fast:<ms>` (the default, `fast:33`: only the damaged rectangle,
at least <ms> between paints) picks the path, for comparing them in one image.

## Engine processes and the heartbeat (issues #861, #862)

The page's WebProcess and the NetworkProcess are WebKit's to start and to replace (it kills a NetworkProcess
that has not answered for 6 s and starts another on demand), so nothing about them may be silent. The UI
process prints, after `WEBPLUS:`:

| Marker | Meaning |
|---|---|
| `PROCESS:LAUNCH:<web\|network>:<pid>` | the launcher forked it (`PROCESS:LAUNCH:<type>:FAIL:errno=<n>`: it could not) |
| `PROCESS:EXIT:<type>:<pid>:status=<n>\|signal=<n>\|gone` | it ended and was collected; `gone`: the kernel has no such id, though the launcher's thread could not wait for it (a child belongs to the thread that forked it until #859) |
| `PROCESS:TERMINATE:<type>:<pid>:group=<e>:pid=<e>` | WebKit's `terminate()`: the errno of `kill(-pid)` and of `kill(pid)` (-1: not tried); 0 is sent; both non-zero (or `-1:-1`) means nothing was signalled |
| `WEBPROCESS:UP:<pid>`, `NETWORKPROCESS:UP:<pid>` | the id WebKit reports changed (a replacement prints its own, every state post is checked) |
| `WEBPROCESS:GONE:<pid>:<reason>`, `NETWORKPROCESS:GONE:<pid>:<reason>` | `memory`, `cpu`, `requested` (the window's own doing) or `crash` (WebKit's C API calls every other reason, an unanswered ping included, a crash) |
| `WEBPROCESS:UNRESPONSIVE:<pid>`, `WEBPROCESS:RESPONSIVE:<pid>` | WebKit's 6 s ping of the page's process went unanswered, or was answered again |
| `WEBPROCESS:RECOVER:<pid>:reload\|stop` | the window's answer to the bar below was taken |
| `HANG:SHOW`, `HANG:HIDE`, `HANG:ANSWER:wait\|reload\|stop` | the window's "page not responding" bar |
| `CRASH:signal=<n>:base=<hex>:rip=<hex>`, `CRASH:rsp=<hex>:addr=<hex>`, `CRASH:at=<hex>` | the UI process crashed (WebKit's `CRASH` is a SIGSEGV at 0xbbadbeef or an illegal instruction, an assertion or a pure virtual call is SIGABRT: the kernel names only the signal): the signal, where it was, the fault address, and the words of the first 16 KiB of its stack that point into its code, for `addr2line -f -C -e <the unstripped lazywebplus-app in the build volume's webkit-port-build/bin> <at>` (the program is not position independent: the words are addresses as they are; some are stale). The handler runs on its own stack, writes by hand and is reset on entry |
| `HEARTBEAT:UP`, `INFO:CPU:GROUPS:ui=<pgid>:web=<pid>:net=<pid>:threads=<n>` | the heartbeat thread started; once, the process groups the CPU shares are summed over (`INFO:CPU:UNAVAILABLE`: the task list could not be read) |
| `CHILD:<name>:<pid>:GONE\|ALIVE:term=<rc>:kill=<rc>:wait=<rc>` | the end: SIGTERM, then SIGKILL of each process group (retried every 100 ms for up to 10 s), then `waitpid`; each rc is 0 or the errno negated, `wait` is the pid it reaped. `QUIT:FAIL` and exit status 3 when one is left. `CHILD:curl:ALIVE` is the ad blocker's list updater, which the window kills at its end |
| `INFO:MAINLAG:<ms>` | WebKit's main thread ran the heartbeat's once-a-second ping 250 ms or more late |
| `STALL:main:<ms>` | the ping has not run after 2 s (again every 5 s while it stays stuck): WebKit's main thread is busy or blocked |
| `STALL:window:<ms>` | one pass of the window's loop has lasted 2 s (`xui_app::stall::pass_age_ms`, every 5 s while it does) |
| `STALL:on_main:<what>:<ms>[:timeout]` | a worker thread waited 1 s or more (or gave up after 3 s) for a call on WebKit's main thread (at most six lines a minute) |
| `INFO:CPU:ui=<pct>:web=<pct>:net=<pct>` | every 5 s when any is 20% or more (at most a dozen lines a minute): the scheduler's task list summed by process group (the launcher makes each engine process its own group; the UI's own group may hold other programs) |

A web process that ended is not destroyed on the WebKit side (patch `0021`; `PROCESS:KEPT:<pid>:<n>` counts them):
releasing the old `WebProcessProxy` when the page is loaded again in a new process aborts the UI process on this
port (`crashDueToCheckedPtrToDeadObject`: a thread-safe CheckedPtr count, the kind `IPC::Connection::Client` has,
that is already zero when a reference is released). What is known: it follows `WKPageTerminate` and Reload of a page
that did not answer; it happens with the embedder's responsiveness calls removed (a build that only toggled a
flag), with a 1.5 s wait before the reload, and with or without the page being unresponsive at the time the
process ended. What is not: which object holds the stale count, whether a NetworkProcess replaced by WebKit's 6 s
timeout (or a process cache or prewarmed process) can hit it, and whether a plain crash does. Each ended
process leaks its proxy (a few kilobytes, a `WebsiteDataStore` reference and an entry in the process map);
`tools/webplus/hang_run.py` ends five and judges the count and the user-memory charge.

Each engine process is started in a group of its own and asks for a SIGKILL when the UI process dies
(`PR_SET_PDEATHSIG`, which the kernel refuses with `EINVAL` until issue #859; the launcher tolerates that), so
the shutdown can kill what a process started, and a replaced NetworkProcess that WebKit's `terminate()`
missed is still on the launcher's list and is ended with the others.

**Page not responding.** When the page's WebProcess does not answer WebKit's ping, a bar above the page says so
(`HANG:SHOW`) with **Wait** (hide it until the page answers and then stops again), **Reload** (end the stuck
process and load the page again in a fresh one: `lzw_engine_recover(1)`) and **Stop** (end it and leave the page
stopped, with a message; Reload or a new address starts it again). Patch `0020` exposes
`WKPageIsWebProcessResponsive`: the C API only tells that the answer changed. The model is `src/hang.rs`
(host-tested), the bar `src/app/hang.rs`, the tests `tests/hang.rs`.

Calls into WebKit's main thread: a worker thread that needs an answer (a toast's id) waits on its own thread and
a slow wait prints `STALL:on_main`; the shell's event channel is polled by a job that is posted and not waited
for (one outstanding at a time), and `confd` is connected to on a thread of its own (`Blocker::attach`), not on
WebKit's main thread, which is where `lzw_ui_profile_dir` first asks for the shared state.

## Testing

`tools/webplus/run.py` ([`tools/webplus/README.md`](../tools/webplus/README.md)) builds the image, serves
LazyWeb's two stand-in sites and the pages of `tools/web/fixtures/theoldnet.com/webplus/` from the host, boots
twice and judges the serial markers, the requests the host saw, the profile on the OS volume and the pixels.
The first boot browses, answers the three dialogs, tries the blocked launches, follows a `mailto:` link, a
`target=_blank` link and a `window.open` button, downloads two files, opens `about:history` and
`about:downloads` by key, scrolls, submits a form, stops a load, goes Back, maximizes the window and quits;
the second boot loads theoldnet.com from the HTTP cache (nothing reaches the host for it), finds the cookie
and the history, clears the history and quits clean.

### The sandbox attack harness

`python tools/webkit/attack_run.py --browser --app lazywebplus` (issue #832's harness, parametrised) boots a
desktop image with `LAZYOS_LAZYWEBPLUS=1 LAZYOS_WEBKIT_ATTACK=1`, in which the two engine programs are a
stand-in that attacks from inside the labels `app:os.lazy.lazywebplus/webprocess` and `.../networkprocess`
(the real UI process starts them), and `python tools/webkit/attack_run.py --kernel` is the kernel-only mode.
The judge's table (`tools/webkit/attackjudge.py`) is the same for both browsers but one row: LazyWeb+'s
network process may write the Downloads folder (the probe `write_downloads`), WebKit's and either page
process may not; `test_attack_judge.py` checks that LazyWeb+'s manifest tables are WebKit's plus that grant.

## Results

`python tools/webplus/run.py --label-trace` (WHPX, 4 GiB guest, a desktop image with networking and the UI
probe, the 8G user memory quota the launchers set): `WEBPLUS:HARNESS:PASS` with every check green: both
boots ran to their end, every page loaded with its title, the three dialogs were answered from the keyboard
and the answers reached the page, the script's `mailto:`, `x-lazyweb:clear-history` and `window.open` with no
click were refused (`LAUNCH:...:BLOCKED`), the click on the `mailto:` link reached `mimed`, `target=_blank` and
a clicked `window.open` opened in the one view, both attachments (300 KiB, and 200 KiB sent over ten seconds)
were saved whole, the host saw the pages, the six resources over HTTPS with SNI, the form's query and the
cookie on the second boot, the profile (history, `Downloads`, `cookies/cookie.jar.db`) was on the OS volume
and Clear history emptied it, the wheel moved the static bands exactly 200 px, the page was laid out again
when the window grew and painted to the right edge when maximized, and both boots closed the window and
reaped both children (`CHILD:web:<pid>:GONE`, `CHILD:network:<pid>:GONE`, `QUIT:PASS`).

| | |
|---|---|
| Load (ms, `WEBPLUS:INFO:TIME`) | 17 navigations: 1 to 262 ms from request to the last byte of the document for most pages (example.com 62, theoldnet.com with its pictures 262, a form 24 to 45); 3.25 s for the script page, which waits for three dialogs; first paint 3 to 119 ms after the load |
| Programs | `lazywebplus-app` **108.0 MiB** stripped static-PIE (113,235,512 bytes); `WebProcess` and `NetworkProcess` 107.4 MiB each; the window's static library is 161 MiB of archive and costs about what W5's does (the seam is shared). The package `os.lazy.lazywebplus-0.1.0.lzp` is **44.7 MB** (deflate); the first boot installs it in about 4.9 s (`PKGD:PROVISION:INSTALL ... ticks=495`) |
| Memory | free RAM of the 4 GiB guest after the first page 3,479 MiB, after 19 navigations 3,404 MiB (`INFO:MEM:used_since_first_load=75`) |
| Labels | `LABEL:ENTER` for both children; **no `LABEL:DENY` beyond the nine optional ones #836 documents** (stat of `/etc`, `/etc/localtime`, `/usr/share/zoneinfo`; `/etc/ssl/openssl.cnf`; fontconfig's baked-in `conf.avail` and its cache writes under `/tmp` and `/system/share/fonts`); the judge (`tools/webplus/denies.py`) fails on any other |
| Sandbox attack harness | `attack_run.py --kernel`: 60 probes as expected; `attack_run.py --browser --app lazywebplus` (the real UI process starts the stand-ins under `app:os.lazy.lazywebplus/<name>`): **60 probes, every one as expected** (every refusal `EPERM` with its `LABEL:DENY`, the network process's `write_downloads` allowed, the page process's refused) |

### Notifications (`python tools/webplus/notify_run.py`)

WHPX, 4 GiB guest, two boots of one image; `WEBPLUS:NOTIFY:HARNESS:PASS` with every check green. First boot:
the page's request at load, with no gesture, is refused (`notify:ready:default:denied`) and opens no bar; a
click on *Ask* opens the bar (`NOTIFY:ASK:https://theoldnet.com`, exactly once) and *Allow* is kept; *Post*
makes a toast the shell attributes to `os.lazy.lazywebplus`, `level=info`, `title=theoldnet.com`, and the same
`tag` a second and a half later updates that toast in place (one `SHELL:NOTIFY:SHOW id=1`, two `POST id=1`);
a click on it runs the page's `click` handler (`notify:clicked:Second title`); a click on a toast of a
page that left in the same click brings the page back (`LOAD` twice); a post that comes up while the window is
minimized shows over the desktop and a click on it restores the window (`XUID:WM:MINIMIZE`, then
`XUID:WM:UNMINIMIZE`) and runs the handler; a burst of six makes three toasts and three `DROP`s, and the
shell's own limit is never hit. Second boot: the allowed site needs no bar, the other origin
(`www.theoldnet.com`) is asked, blocked and shows nothing, `about:lazyweb` lists both decisions and *Forget*
removes the blocked one (`notify.tsv` on the OS volume is exactly `https://theoldnet.com<TAB>allow<TAB>...`),
and both boots close and reap both children. What WebKit refuses before the UI process hears of it (a request
without a gesture, a post before the permission) is judged from the page's own titles; `DENY:...:gesture` and
`:not-allowed` are covered by the host tests only.

## Not verified, and known limits of what was

* **Download progress while a download runs.** The status bar showed "0 bytes of 200.0 KB" four seconds into a
  ten-second download: the engine's `didWriteData` did not arrive before the end on a slow attachment, so the
  progress bar jumps from 0 to done (the file, its size and the rename are verified).
* **Notifications** were verified by `tools/webplus/notify_run.py` (see *Notifications*); a service worker's
  `showNotification`, the page's `icon` and `image`, actions, sound and a system-wide list of sites are not done.
* Not tried: HiDPI, a WebProcess crash in this window (W5's *Failures* path is shared), IME, the pasteboard,
  Settings > Default apps by a session (the preference is covered by `deskmenu` host tests and `mimed`'s
  boot self-test), a real PC.
* The harness's stand-in sites are LazyWeb's (`tools/web/sites.py`): nothing was loaded from the internet.

## `os.lazy.webkit` is not retired

The issue allows retiring the W5 browser once this harness passes everything W5's did. It was not done: LazyWeb+
is the same engine with LazyWeb's window, W5's `app_run.py` harness has not been run against it page for page,
and the W5 package is the smaller program for anyone who wants the engine with no chrome. The two stay; the
retirement (the package, `LAZYOS_WEBKIT`, `app.py` without `--plus`, `xui-app/crates/webkit`'s window feature)
is a follow-up once someone decides that.
