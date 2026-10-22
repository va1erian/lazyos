# LazyWeb+, the browser on WebKit

`os.lazy.lazywebplus` ("LazyWeb+" in the menu) is the web browser of LazyOS: a window with menus, history and
downloads whose page is drawn by WebKit: JavaScript, forms, cookies, the modern web. The engine is not in the
window. It belongs to **webd** ([`webd.md`](webd.md), [`webd-plan.md`](webd-plan.md)), the session's WebKit
daemon, and LazyWeb+ is one of its clients (phase P4, 2026-10-22). It began as a second browser beside
LazyWeb, a smaller one on the pure-Rust Blitz engine with no script engine; LazyWeb and Blitz were removed
(webd plan P3) and this is the only browser now. Issue #835.

```bash
python tools/run_demo.py --lazywebplus    # desktop + networking + HTTPS + webd + the browser (builds WebKit first: Docker)
python tools/webplus/run.py               # build, browse the stand-in sites, judge (tools/webplus/README.md)
cd xui-app && cargo test -p xui-webplus -p xui-webkit -p xui-webkit-remote   # the window, the seam and the remote engine, host
python tools/webplus/test_judge.py        # the judge fails when it should; the sessions are current
```

## What it is

A plain xui app: the binary `xui-lazywebplus` (crate `xui-app/webplus`, built by `python tools/xui/build.py`
into `target/xui/xui-lazywebplus.elf` with the other apps) and the core package
`xui-app/packages/lazywebplus`. It links no WebKit, no curl and no OpenSSL; WebKit's UI process, the
`WebProcess` and the `NetworkProcess` are webd's, and so are the engine's sandbox labels
(`app:os.lazy.webd/webprocess|networkprocess`). The package is shipped by `LAZYOS_LAZYWEBPLUS=1`, which needs
`LAZYOS_DESKTOP=1`, `LAZYOS_NETD=1` and webd (the OS volume defaults to 1 GiB).

The window sits on the engine seam of `xui-app/crates/webkit` (`xui-webkit`: the `Engine` trait, the mailbox
between the engine and the window, the generic page view, the pure helpers). The seam is host-tested against a
recording engine. The real engine is `RemoteEngine` in `xui-app/crates/webkit-remote` (`xui-webkit-remote`),
which implements `Engine` over `xui_webd_client::FullWebView`:

* The page is **one full view** of webd (`os.lazy.webd.view.v1`), made through the browser scope
  (`os.lazy.webd.browser.v1`) in webd's `browser` profile, `$HOME/.apps/os.lazy.webd/profiles/browser`: cookies,
  HTTP cache, site storage and the content-blocking store. The view is made when the page area first has a size,
  and `ViewConfig` (`browser_config`) asks for scripts, the web, hand-offs asked of the window (`Handoff.Ask`),
  downloads, pop-up requests, frames with damage, dialogs, blocking, notifications and `notify_in_client`.
* **Frames** go from webd's shared slots straight into the mailbox (`FullWebView::poll_into` with a
  `PixelSink`: one copy). The view attaches three frame slots and two band slots (`SlotPlan`), most needed first,
  and runs with fewer when the process cannot have more (the per-process shared-buffer limit,
  `limit.shared_buffer_max`, which is now eight screens: at 2560x1440 a frame slot is 14.7 MB).
* The window's loop **parks on the view's event channel** through an endpoint descriptor (`watch_fd`), so it reads
  the view the moment webd writes; its timers cover only a pointer move waiting its turn (8 ms) and a slow beat
  (250 ms) for messages webd's full inbox refused.
* **Pointer moves** are merged in the client: at most one every 8 ms, the newest waiting (issue #890).
* Everything runs on the process's first thread, the window's, because a Messenger handle belongs to its thread
  and the shell's notification service accepts only the process's first thread. The toasts' worker, the downloads'
  notifier and the ad blocker's worker hand their calls back to it (`platform/main_thread.rs`,
  `xui_webkit::relay`).

The window has a menu bar (File, View, History, Downloads, Help), the toolbar (Back, Forward, Reload which turns
into Stop, Home, the address field, an "open in the OS" button and the throbber), the status bar (hover link,
what the engine is doing, running downloads with a progress bar, the padlock) and the same keys (Ctrl+L, F5,
Alt+Left, Alt+Right, Alt+Home, Ctrl+H, Ctrl+J, Ctrl+S, Ctrl+W, Esc to stop while a page loads: Enter and Esc are
the address field's only while it has the focus, because a real engine has pages that use them). The window
title is `<page title> - LazyWeb+`.

## What moved where

| Concern | In the window (before P4) | Now |
|---|---|---|
| Other schemes (`mailto:`, ...) and `x-lazyweb:` commands | `policy::decide` on WebKit's thread | webd holds the navigation (`Handoff.Ask`, `NavigationRequested`); the client refuses it at once (`NavigationAnswer(false)`) and decides with its own `policy.rs` and its real-input lock |
| Downloads | the window picked `$HOME/Downloads/<name>.part` | webd names and writes the file (`lazydownloads`: `.part`, renamed when every byte is in); the window lists the downloads and toasts them |
| Notification permission and toasts | provider in the UI process, bar and toasts in the window | webd keeps the profile's decisions and hands the page's notifications to the client (`ViewConfig.notify_in_client`: `NotifyShow`, `NotifyCancel`, `NotifyGone`); the bar and the toasts stay the client's, so a click raises the browser's window; the client answers with `NotifyState` |
| The person's notification decisions | `notify.tsv`, read by WebKit | still `notify.tsv`, the client's; told to webd each time a view is made (webd keeps them only while it runs); forgetting one is `NotifyForget` |
| Content blocking | the engine's store in the window's profile; switch, exceptions and the one-shot "Reload without blocking" in the window | the lists are the client's (it downloads and converts them); webd compiles what it is given (`BlockingCompile`) into the `browser` profile's store; the switch and the exceptions are webd's per-profile `SetBlocking(enabled, exceptions)` (at most 64 sites), and the one-shot is an exception until the browser leaves the site |
| The hang bar (Wait, Reload, Stop) | `lzw_engine_recover` | `Recover(view, reload)`; Stop ends the page's process, webd reports the view lost and the page is blank until the next load, which makes a new view |
| A page whose process crashes | the window said so | the page is blank until the next load (a new view) |
| webd going away | (not possible) | the status says "The web engine (webd) is not running" or "stopped; reconnecting", the client retries every second and loads the page again |
| Engine health, heartbeat, process launcher, crash handler | the UI process and the window | webd's (its watchdog covers WebKit's thread); markers are `WEBD:NOTE:<line>` |
| Sandbox of the page and network processes | `[processes.webprocess]` and `[processes.networkprocess]` of the browser's manifest | webd's manifest; LazyWeb+ keeps `[processes.listupdate]` only |

The pages of the browser itself (`about:history` and the others) stay `data:` URLs built in the window. One
longer than 8 KiB (webd's address limit) is given with `LoadHtml` at a short base address carrying a random
number, and its address is told back to the window as the original `data:` URL (`webkit-remote/src/alias.rs`),
so the window still recognises its own page. Back or Forward to such a page that WebKit must load again shows an
empty page; Reload builds it again.

## Navigation policy: a script never starts anything

A browser without a script engine (LazyWeb, now removed) could take it for granted that every launch it
reports is the user's. WebKit cannot. webd loads `http:`, `https:`, `data:`, `blob:` and `about:` itself and
holds every other navigation, redirect and window request (`Handoff.Ask`); the client answers "no" to webd at
once and decides for itself with `policy::decide(url, kind, user_gesture, from_own_page)`
(`xui-app/webplus/src/policy.rs`, pure and host-tested), with the gesture flag from WebKit
(`WKNavigationActionHasUnconsumedUserGesture`: a click or a key, not a timer, a load event or a redirect):

| URL | Decision |
|---|---|
| `http:`, `https:`, `data:`, `blob:`, `about:` | webd loads it |
| `file:` | never loaded (webd refuses it; see *Known limits*) |
| `x-lazyweb:` (the browser's own commands) | runs only from one of the browser's own pages, for a click on a link there, and never goes to another app; anything else is refused |
| `mailto:`, `tel:`, any other scheme | handed to `mimed` only when a user gesture started it; otherwise refused and reported (`WEBPLUS:LAUNCH:<url>:BLOCKED`) |

**A second lock.** WebKit's gesture flag is one boolean from a large engine, and a click may authorise more than
one thing (a timer chain, a redirect), so a hand-off to another program also needs real input the window itself
forwarded to the page within the last three seconds, and uses it up (`xui_webkit::gesture`): one click, one
hand-off. A hand-off without it is refused and reported like a script's. The lock lives in the client, where the
input is; webd's own `Handoff.Browser` mode is not used.

An address the user types is opened by the window itself (`handoff.rs`), so a navigation to `mailto:` with no
gesture (kind `Other`) is a script's, a redirect's or a bug's, and is refused. "On one of the browser's own
pages" is an exact comparison with the page the window built (`Internal::name_for`), so a web page that looks
like `about:history` and links to `x-lazyweb:clear-history` is not one. `window.open` and `target=_blank` load in
the one view, and only when a gesture started the request (`WEBPLUS:POPUP:<url>`); a script that opens a window
on its own gets nothing. The harness page `webplus/script.html` tries a `mailto:`, an `x-lazyweb:clear-history`
and a `window.open` with no click, and the judge requires all three to be refused.

## Pages

`about:history`, `about:downloads` and `about:lazyweb` and the start page are HTML built in the window and
shown as `data:` URLs (so there is one rendering path), reached by the menu, Ctrl+H and Ctrl+J. History is
`$HOME/.apps/os.lazy.lazywebplus/history.tsv` (the newest 1000 pages that finished loading over `http:`,
`https:` or `file:`), cleared from `about:history`.

## Downloads

A response the view cannot show, or one sent as an attachment, is downloaded by webd: it names the file (the
name made safe, unique among the files there and the downloads under way, ` (1)`, ` (2)`) and its network
process writes `$HOME/Downloads/<name>.part`, renamed when the download completes and removed when it fails or
is cancelled. The window gets `DownloadStarted`, `DownloadProgress`, `DownloadDone` and `DownloadFailed`,
cancels with `DownloadCancel` and reads the finished file's size from the folder (`read:$HOME/Downloads` in its
manifest). The status bar shows the progress while it runs and `about:downloads` lists the session's downloads
with open and cancel links. File > Save Page As Download saves the current page the same way (`Download`). A
download needs no gesture (an attachment is a response, not a click), so a page can start them: a session is
capped at 256 downloads, a name that is another download's `.part` is never reused, a file that appeared at the
final name meanwhile is never replaced (the download fails instead), invisible direction characters are stripped
from names, and with no home folder nothing is saved. Each download also asks for a toast (`os.lazy.notify`).

## Dialogs

`alert`, `confirm` and `prompt` arrive as `Dialog` events and are answered with `DialogReply`. The window draws
them with xui's `Dialog::message/confirm/prompt` ("theoldnet.com says"), modal to the page area. webd
dismisses a dialog without asking when a view raises more than three in ten seconds.

## Notifications

A page's `Notification` API works: `Notification.requestPermission()`, `new Notification(title, {body, tag})`,
and the `show`, `click` and `close` events. The engine half (the WebKit provider and the profile's permission
record) is webd's; the browser's half, which webd hands over with `ViewConfig.notify_in_client`, is the bar, the
choices and the toasts: the model is `xui-app/webplus/src/notify.rs` and its parts, with no window and no engine
needed to test it. The shell's toasts are `os.lazy.notify` ([`notify-plan.md`](notify-plan.md)). Why the client
posts them: the shell raises the app that posted a toast when it is clicked, and an app cannot raise another
app's window, so only the browser can bring the browser back.

* **Permission is per origin, asked once, kept.** A request (`NotifyPermissionRequest`) starts a bar above the
  page (the browser's own chrome, not a toast: a toast never takes the keyboard, and an answer must never depend
  on one) naming the host with *Allow*, *Block* and *Not now*. Allow and Block are written to
  `$HOME/.apps/os.lazy.lazywebplus/notify.tsv` (`origin<TAB>allow|block<TAB>seconds`, the origin as WebKit
  writes it) and told to webd (`NotifyPermission`). webd keeps the decisions only while it runs, so the browser
  tells it all of them each time a view is made. *Not now* refuses the pending requests and keeps nothing: the
  site may ask again. A request that came without a user gesture is refused outright (WebKit does it before webd
  hears of it; the window looks at the input it forwarded as well), and so is one for a page that is not `http:`
  or `https:`. One site is asked at a time, four at most wait, and a bar goes when its page does. `about:lazyweb`
  lists the decisions with a *Forget* button each: `NotifyForget` makes the page's `Notification.permission`
  `default` again and the site's live toasts go.
* **Identity.** The toast is posted by the browser, so the shell shows it under **LazyWeb+**, and the title is
  the host (`example.com`, `host:8443`: never the page's words; a host too long for it keeps its end). The page's
  `title` and `body` go in the toast's body as two lines, control, bidirectional-override and zero-width
  characters removed, cut to the shell's limits (64 and 256 characters). The level is always Info, the icon the
  app's own, there is no action and the timeout is the level's, so nothing a page sends can look like a download
  or a system warning.
* **`tag` replaces in place**: the same tag from the same site updates the live toast (`replaces_id`), and the
  page's earlier object gets its `close`.
* **A click brings the page back.** The shell raises the app's windows on a click (`ActivateApp(label,
  session)`, shell-only), the window then answers `NotifyState(Clicked)` so the page's `click` handler runs, when
  its document is still there. A notification of a page that has gone (`NotifyGone`; the toast stays) opens the
  page's address in the one view, when it is a page of the notifying site (otherwise the site's front page); the
  old handler cannot run, there is no document for it. A toast the person dismisses or that times out is a
  `NotifyState(Closed)`, a `close` event for the page.
* **Limits are per site** ([`live.rs`](../xui-app/crates/webnotify/src/live.rs) in `lazywebnotify`, the crate
  that also holds the toast's title and body rules and is shared with webd): 3 live toasts, a burst of
  3 posts and one more a second; 10 live web toasts in all, so the shell's 16 and its burst of 6 are never used
  up by a site, and a download's toast always has room. What a site posts beyond that is dropped
  (`WEBPLUS:NOTIFY:DROP:<origin>:<why>`), not queued, and answered as closed so the page's object does not stay
  "showing". A toast outlives its page until its timeout; closing the browser takes its toasts down.
* **A notification is shown only when its origin is the origin of the page on screen**, and a request opens a bar
  only for that page's own origin; anything else is `DENY:<origin>:page`. A notification from a cross-origin
  frame is therefore refused.
* **Nothing persists but the permissions**: no notification history (the shell keeps none either). `notify.tsv`
  is in the browser's own folder, which webd's processes cannot write (they write `$HOME/.apps/os.lazy.webd` and
  `$HOME/Downloads`).
* **In memory only mode** (below) the view is made without notifications: webd refuses them for a profile that
  is not on disk, so pages cannot notify.

The shell calls are made on the window's thread, the process's first (the notification service names an app by
the task id of the sender, and a message from another thread is refused as "not an app"); the toasts' worker
waits on the shell channel's readiness descriptor (`notify::Watch`, docs/architecture/endpoint-fd.md) and asks
the window to make the call only when the channel has news (issue #893). The shell's own half: the compositor's
`ActivateApp` (`idl/display.midl`, from the label and session the kernel stamped on the notification calls), a
toast body keeps its line feeds, and the platform client (`xui_app::platform::notify`) can `Listen`.

Markers (UI-probe images only: they name sites), after `WEBPLUS:NOTIFY:`: `ASK:<origin>`,
`ALLOW|BLOCK|DISMISS:<origin>`, `SHOW:<origin>:<id>`, `CLICK:<id>`, `CLOSE:<id>`, `DROP:<origin>:<why>`,
`DENY:<origin>:<why>`, `FORGET:<origin>`. Verified by `python tools/webplus/notify_run.py`
([`tools/webplus/README.md`](../tools/webplus/README.md)) and the host tests of `xui-app/webplus`
(`tests/notifications.rs`, `notify/*`) and `lazyshell`.

## The profile

Cookies, the HTTP cache and site storage are in webd's `browser` profile
(`$HOME/.apps/os.lazy.webd/profiles/browser`, [`webd.md`](webd.md) "Profiles"), made by webd and written by its
network process; a second run of the app finds the first one's cookie. The browser's own folder,
`$HOME/.apps/os.lazy.lazywebplus`, holds what is the client's: `history.tsv`, `notify.tsv`, the ad blocker's
lists (`lists/`) and the `page-data-in-memory` switch.

**Page data in memory** (issue #940): *File > Keep Page Data in Memory (on/off)* writes or removes
`page-data-in-memory` in the browser's folder (`xui-app/webplus/src/memory.rs`); at the next start the view is
made with `ephemeral`, webd's `browser` profile in memory: cookies and site data are not kept and are gone when
the view is, and nothing is written to the profile folder. webd refuses content blocking and notifications for a
profile in memory, so in that mode **the ad blocker is off and pages cannot notify** (before P4 the blocker kept
working). The history, the notification choices and the ad blocker's downloaded lists stay in the browser's
folder. Off by default; it is meant for a `/home` on a USB stick, where every write of the network process
stalled the page. Turning it on does not delete what earlier runs kept: that stays in the profile, unused, until
it is turned off again.

The bundle's SQLite is built with `SQLITE_OMIT_WAL` (`tools/webkit/recipes_text.py`): WebKit opens its databases
with write-ahead logging, whose index is a mapped `-shm` file, and LazyOS has no file-backed `mmap`, so without
it no cookie was ever stored (`webkit/README.md`).

## `mimed`: which browser opens a link

`mimed` keeps one app per `(type, verb)` and the last to register wins, so with two browsers both registering
`x-scheme-handler/http`, `x-scheme-handler/https` and `text/html`, the one installed last would open every link
(it was LazyWeb and LazyWeb+; LazyWeb+ is the only browser now, and the preference stays for a second one). The
confd key `user/<uid>/mimed/default/<type>` (a string holding an app id; `libs/deskmenu/src/defaults.rs`) is
written by **Settings > Default apps** (all three types at once) and honoured by `mimed` for the uid the kernel
stamped on the `Open`, only while that app still holds a registration for the type, so a stale choice (an
uninstalled browser) falls back to the registry. `confd` lets a uid touch only `user/<uid>/**`. With two browsers
installed and no choice made, `mimed` keeps its old rule (the app registered last wins, which is install order).

## Blocking ads

LazyWeb+ blocks the requests the community filter lists mark as ads (issue #845), so a page loads less: a
blocked resource is never fetched, its script never runs and its box is never laid out. v1 ships **EasyList**,
network blocking only (no element hiding), fetched at runtime and never part of the image.

**The mechanism is WebKit's content extensions** (Safari content-blocker rule lists): JSON compiled to a bytecode
automaton, stored on disk, shared with the web processes and enforced before a request leaves. The pieces:

* **The engine** (webd's): `ENABLE_CONTENT_EXTENSIONS` is on in `OptionsLazyOS.cmake`; patch `0019` supplies
  `NetworkCache::Data::tryCreateSharedMemory()` for the curl backend, and patch `0018` makes the C API report a
  *blocked* load. webd holds the store (`<profile>/contentextensions`) and the per-navigation decision
  (`webkit/port/app/blocking.cpp`).
* **The converter**, `xui-app/crates/adblock` (`lazyadblock`, MIT, no dependencies): Adblock Plus syntax to
  rule lists. It emits only what WebKit's own parsers accept (one refused pattern fails a whole list, so it is
  strict and its tests parse every output with an independent parser of WebKit's URL-filter subset), splits at
  the 150,000-rule limit repeating the exceptions in each part (an exception only reaches back inside its own
  list), and drops what WebKit cannot say exactly, counting it, never widening it. `README.md` of the crate has
  the table of what becomes what and what is dropped.
* **The window's half**, `xui-app/webplus/src/blocker/`: the user's settings, the lists and the worker that
  fetches and converts them, the calls to webd (`BlockingCompile`, `BlockingLoad`, `BlockingForget`,
  `SetBlocking`) and `about:blocker`.
* **The fetch**, `curl` (the `nettls` program, `LAZYOS_TLS=1`: `run_demo.py --lazywebplus` and the launcher
  bring it), run by the window as a child under a label of its own, `app:os.lazy.lazywebplus/listupdate`
  (`[processes.listupdate]` in the manifest): outbound network, the resolver, the host table and the CA bundle,
  and write access to `$HOME/.apps/os.lazy.lazywebplus/lists`, nothing else. It sends no cookie and no referrer
  (curl keeps none), so a list server cannot tie a request to the browsing profile. A conditional request (the
  list's `ETag` and `Last-Modified`) is made at most when the list's own `! Expires:` line says so (bounded
  from one hour to fourteen days; EasyList says four days), at most once an hour after a failure, and on
  *Update now*. The lists are the browser's: webd compiles what it is given and does not download.

### How a list becomes a block

1. The worker reads `lists/<id>.txt` and its record `lists/<id>.meta` (the source's hash, the engine's ids of
   the compiled parts, the ETag, when it was fetched and tried).
2. A source whose hash is the record's is **not converted**: webd is asked to attach the parts its store holds
   (`BlockingLoad`, which costs a millisecond or so for EasyList); only when the store has none (a new profile, a
   WebKit that changed its format) is the text converted and compiled (`BlockingCompile`, seconds on a fast host).
   The result comes back as `BlockingList`.
3. Parts are named `<list>-<hash of the source>-<n>`: a new version of a list is a new name.
4. A new version is **tried first**: its parts are compiled while the old ones keep blocking, and only when the
   engine has taken every part does it replace `lists/<id>.txt` and the record, and the old parts are forgotten.
   A version the engine refuses (one pattern its parser does not take fails a whole list) changes nothing: the
   old list keeps blocking, the refused version's validators are not kept, and it is tried again at the next
   due time (`WEBPLUS:BLOCK:REFUSED:<id>:...`).
5. A failed or broken download (not HTTP 200, no `[Adblock` header, too short) changes nothing either. A list
   nobody could fetch yet shows why on `about:blocker`. A list over 16 MiB, or one that becomes more than
   500,000 rules, is refused whole.
6. With the blocker switched off **nothing is fetched, loaded or compiled**; switching it on again does the work.
7. A page loads **unblocked until its list is ready**; nothing waits for it.

### Switching it off when a page breaks

* **This site**: *Turn off for <site>* on `about:blocker` (the toolbar's shield opens it) excepts the site and its
  subdomains for good. The window tells webd the exceptions (`SetBlocking`, at most 64 sites; the person's first
  ones when there are more) and webd takes the lists off a main-frame navigation to such a site and puts them back for the
  next one, so nothing is recompiled.
* **Once**: *Reload once without blocking* (also View > Reload Without Blocking) loads the page whole until the
  browser leaves its site; nothing is remembered. It is an exception sent to webd for as long as the browser stays
  on the site.
* **Everything**: *Turn ad blocking off*, and a list can be switched off by itself.

The switches are `confd` keys under `user/<uid>/lazywebplus/blocker/` (`enabled`, `exceptions`, `lists_off`): the
user's, out of the engine's processes' reach. Without `confd` they last for the run. The price is
`os.lazy.confd.v1` in the app's permissions. `x-lazyweb:` commands of `about:blocker` obey the rule of the other
internal pages: only from that page and only for a click.

### What it does not do

Element hiding (the part of a list that hides boxes: `##`, `#@#`, the `$elemhide` family; it is counted and not
used), `$redirect`, `$removeparam`, `$csp`, scriptlets (code injected into pages: not in v1), regexes WebKit's
parser cannot take (alternation, `{n,m}`, `\b`), non-ASCII patterns, and `$domain=` with both included and
excluded domains. A trailing `^` matches a separator character and not the end of the address, so such a filter
can only block *less* than Adblock Plus would. More lists (EasyPrivacy, Peter Lowe's, uBlock's), element hiding
and a user filter editor are separate issues.

### Results (issue #845; measured with the engine in the browser's process, before P4)

* **The real list** (EasyList of 13 Oct 2026, 82,058 lines): 56,918 rules (56,331 blocks, 587 exceptions) in 12 MB
  of JSON, 0.11 s to convert in release on a fast host. Not used: 24,822 element-hiding lines, 18 lines with an
  option WebKit has no trigger for, 19 patterns outside WebKit's regex subset, 3 exceptions (which would cancel a
  block, so those blocks stay: `dropped_exceptions`), 8 duplicates.
* **Through WebKit itself** (`python tools/webkit/blockcheck.py --list ...`, the port's `webkit-smoke` in rules
  mode, 20-CPU Docker host): the list is accepted; `WKUserContentExtensionStoreCompile` 2.8 s, process peak
  resident set 391 MB; read back from the store in under 2 ms (what every later start pays); on a page with two
  EasyList-matching requests and one clean one, 2 loads blocked, 0 on a clean page.
* **In the guest** (the harness's small list, four boots of one image): the `listupdate` child fetched the list
  under its own label (no `LABEL:DENY` beyond the documented optional ones), it was converted, compiled by
  WebKit, shared with the web processes and attached; a later boot loaded it from the store with no conversion; a
  page loaded with the ad picture, the tracking pixel and the ad script **never requested** (the host saw each
  exactly once, after the user excepted the site); *Update now* sent the ETag and was told 304; the exception and
  the compiled list outlived a boot. With the real EasyList: fetched (2,150,374 bytes), converted in about a
  second, compiled and attached in about 4.3 s, with the browser up throughout. Memory of that compile in the
  guest and page-load times on real pages, with and without blocking, were **not** measured. Rerun against webd,
  the harness's blocker boots judge the same behaviour; the numbers above were not repeated.

## Permissions

The app: `os.lazy.display.v1`, `os.lazy.display.cursor.v1`, `os.lazy.input.v1`, `os.lazy.mimed.v1`,
`os.lazy.notify.v1`, `os.lazy.confd.v1` (the blocker's switches), `os.lazy.clipboard.v1`,
`os.lazy.webd.view.v1` (every call on the view) and the browser scope `os.lazy.webd.browser.v1` (the view is
made through it: the `browser` profile, downloads, content blocking), outbound network (only for the list
updater below: a child label holds at most its app's), `write:$HOME/.apps/os.lazy.lazywebplus` and
`read:$HOME/Downloads`, derived from a `--label-trace` run of the harness. The ad blocker's `listupdate` child
(`curl`) holds the network, the resolver, the host table, the CA bundle and
`write:$HOME/.apps/os.lazy.lazywebplus/lists`, and no interface.

The engine's two processes are webd's, sandboxed as before ([`architecture/process-sandbox.md`](architecture/process-sandbox.md),
#836): the page process reads fonts and nothing else; the network process reads the resolver, the host table and
the CA bundle, uses outbound sockets and writes webd's folder and `$HOME/Downloads`. The window is an ordinary
app: its file and network entries are consent text, not enforced. What a compromised page process can do through
webd is limited by webd's own checks ([`webd-plan.md`](webd-plan.md) section 5). The network process can read
every profile's cookies, as the plan states.

## Licence

`xui-webplus` and `xui-webkit` are declared `GPL-2.0-only OR GPL-3.0-or-later` (the window began as a fork of
LazyWeb, which was `GPL-2.0-only`), and the GPLv2-compatible registry tree is still checked
(`python tools/nettls/licenses.py --lazywebplus`, one waiver: `unicode-linebreak`, Apache-2.0 only, in every xui
app through `xui-canvas`; `python tools/webkit/licenses.py` checks that the window crates keep both licences and
name no OpenSSL crate), until a maintainer relicenses them (webd plan, open question 8). The program no longer
links WebKit or OpenSSL (webd does), but it links two crates of its own declared `GPL-3.0-or-later`
(`xui-webkit-remote` and `xui-webd-client`), so it is distributed as GPL-3.0-or-later. `THIRD_PARTY.md` has the rows.

## Upgrading: what is kept and what is lost

An existing OS volume that had the in-process browser keeps its files in `$HOME/.apps/os.lazy.lazywebplus/`:

* **Kept** (they are the client's): `history.tsv`, `notify.tsv` (the notification choices), the ad blocker's
  lists and its settings (`confd`), and the Downloads folder.
* **Lost**: the cookies, the HTTP cache and the site storage the old engine kept in that folder. They are not
  migrated: webd's label cannot read that folder and the client cannot write webd's. Sites need a new sign-in.
  The old engine's files stay in the folder, unused.

## Known limits

* **`file:` URLs do not open.** webd never loads `file:`, so a local HTML file opened from Files fails with an
  error. The plan's answer (the client reads the file and passes the bytes with `LoadHtml` and a base address,
  webd-plan open question 5) is not built.
* **No tabs.** Each tab would be a view of the same (client, profile) group, sharing one web process, hidden ones
  `SetVisible(false)`; the web process cap counts (client, profile) pairs, so tabs would not reach the cap of 4.
  What is missing is the window's chrome for tabs, a way to stop each closed tab leaving a parked proxy in webd
  (webd-plan P5 pooled light views only: a browser view is a full view, and tabs would be related pages), and a way to recover a hung tab (`Recover` refuses a process other
  views share, so a hung tab would need its group's views made again). `window.open` and `target=_blank` open in
  the one view. Tabs, windows, bookmarks, an omnibar, data controls, a password vault and issue reports are
  planned in [`lazywebplus-plan.md`](lazywebplus-plan.md).
* **A notification permission leaks across profiles.** WebKit keeps one permission record for the whole context,
  so a site allowed in the browser profile reads as `Notification.permission == "granted"` in another app's
  profile, where webd then refuses the notification (it posts nothing for a profile that did not allow it).
  Documented, not fixed.
* **Back to an older internal page.** The window remembers the newest `data:` URL of each of its own pages, so Back
  to an older History or Downloads page is no longer "its own": its buttons answer "Blocked: the page tried to open
  another app" (safe, but useless) until the page is opened again. A page longer than 8 KiB that WebKit loads
  again shows empty (above).
* **Dialogs** are titled with the top-level site, not the frame that asked.
* **Evidence lines** go through `xui_app::trace` (`Channel::Webplus`, `marker()` in `lib.rs`) except four
  `println!`s left in `app/events.rs` (`STOPPED`, `FAIL`, `DIALOG`); the ones that name a page, an address or a
  file print only in a `LAZYOS_UI_PROBE=1` image.
* **`history.tsv` is in the browser's folder** (a plain file the window reads: the last 2 MiB of it, titles cut at
  512 characters, addresses over 2048 dropped). webd's processes cannot write it.
* **`Notification.close()` during a navigation leaves the toast up.** A page that enters the back/forward cache is
  suspended and WebKit closes its notifications exactly as `close()` does (the provider cannot tell them apart),
  so a close that arrives while a main-frame navigation is loading is taken as the suspension: the toast stays
  until its timeout, as the notification of a page that is gone.
* **The page area is drawn at device scale 1** (small text at 2x), no find in page, no context menu, no pasteboard
  beyond WebKit's own, no IME, no WebGL, video or WebAssembly.
* **The ad blocker's compile runs in webd's UI process**; a hostile list that stays inside the bounds (16 MiB,
  500,000 rules) still costs seconds of one core and a few hundred MB while it compiles. Regular expressions that match the empty string are refused by the converter; nested quantifiers
  are not detected. The lists live in the browser's folder, which no engine process can write; the compiled store
  is webd's.
* **A host the site list cannot spell** (an IPv6 literal) is blocked like any other but cannot be excepted by name.
* **The filter lists need `/system/bin/curl`**, which `LAZYOS_TLS=1` ships (`run_demo.py --lazywebplus` sets it).
  A build with `LAZYOS_LAZYWEBPLUS=1` alone browses and blocks nothing, and `about:blocker` says "curl did not run".
* The blocked count of `about:blocker` and the shield is for the web page on show or browsed last (opening
  `about:blocker` does not reset it); a load a script makes in a loop is counted as often as it is blocked.
* The prompt dialog does not seed the page's default value.
* **webd, not the browser, is the single point of failure.** If webd stops, the page is blank and the status line
  says so until it is back; the history, downloads list and settings stay in the window.

## Markers

The window's lines are `WEBPLUS:*` (channel `Webplus`): `UP:PASS` at the first frame, `WAIT:<errno>` while the
compositor is not there, `QUIT:PASS` when the window closed and nothing it started is left (the list updater's
`curl`; `CHILD:curl:ALIVE` otherwise), `INFO:RESIZE:<w>x<h>` when a picture of a new size arrives, the page and
navigation lines (`LOAD`, `TITLE`, `NAV`, `FAIL`, `POPUP`, `LAUNCH:<url>:OK|FAIL|BLOCKED`,
`DOWNLOAD:START|DONE|FAIL`, `COMMAND:...:BLOCKED`, `STOPPED`), `HANG:SHOW`, `HANG:HIDE`, `HANG:ANSWER:wait|reload|stop`,
`BLOCK:*` and `NOTIFY:*`. The engine's lines (`PROCESS:*`, `WEBPROCESS:*`, `NETWORKPROCESS:*`, `CHILD:web|network`,
`INFO:PAINT`, `STALL:*`, `CRASH:*`) are webd's and arrive as `WEBD:NOTE:<line>` in webd's console output
(`docs/webd.md`, "Markers"); webd prints the repeating ones (`INFO:*`, a hung page's pings) under a budget of its
own, 120 per ten seconds, apart from the 20 per ten seconds it allows for lines a client can cause. The heartbeat
thread, `HEARTBEAT:*`, `INFO:CPU:*` and the UI process's crash line belonged to the old in-process host and are
gone; webd's watchdog covers WebKit's thread.

**Page not responding.** When the page's process does not answer WebKit's ping, webd reports `Responsive(false)`
and a bar above the page says so (`HANG:SHOW`) with **Wait** (hide it until the page answers and then stops
again), **Reload** (`Recover(view, reload = true)`: end the stuck process and load the page again in a fresh
one) and **Stop** (`Recover(view, false)`: end it; webd reports the view lost, and the page is blank with a
message until the next load makes a new view). The model is `src/hang.rs` (host-tested), the bar
`src/app/hang.rs`, the tests `tests/hang.rs`; the harness is `python tools/webplus/hang_run.py`. A web process
that ended is not destroyed on the WebKit side (patch `0021`), so each leaves a parked proxy (webd's reuse pool of
P5 takes light views only, never a browser's full view); the reason and the numbers are in `webd-plan.md` ("What P1
found", "P5 as built") and `tools/webplus/hang_run.py` judges the count.

## Testing

`tools/webplus/run.py` ([`tools/webplus/README.md`](../tools/webplus/README.md)) builds the image, serves the
two stand-in sites and the pages of `tools/webtest/fixtures/theoldnet.com/webplus/` from the host, boots and
judges the serial markers (`WEBPLUS:*` from the browser, `WEBD:NOTE:*` for the engine), the requests the host
saw, the profile on the OS volume and the pixels. The first boot browses, answers the three dialogs, tries the
blocked launches, follows a `mailto:` link, a `target=_blank` link and a `window.open` button, downloads two
files, opens `about:history` and `about:downloads` by key, scrolls, submits a form, stops a load, goes Back,
maximizes the window and quits; the second boot loads theoldnet.com (its cookie and the history are found, the
history is cleared) and quits clean; a third and a fourth boot judge the ad blocker. `notify_run.py` (web
notifications, two boots) and `hang_run.py` (a page that hangs: the bar, Wait, Reload, Stop) are the other two
harnesses; `--label-trace` fails on any `LABEL:DENY` beyond the documented optional ones. All three judge
LazyWeb+ as a client of webd. The host tests are `xui-webplus` (`tests/`: the window offscreen against a
recording engine), `xui-webkit` and `xui-webkit-remote` (`tests/remote.rs`: the real engine's code against
`xui-webd`'s in-memory fabric).

The webd side of the sandbox is attacked by `python tools/webkit/attack_run.py --browser` (its `--app` is `webd`)
(issue #832's harness, parametrised): a desktop image with `LAZYOS_LAZYWEBPLUS=1 LAZYOS_WEBKIT_ATTACK=1`, in
which the two engine programs are a stand-in that attacks from inside the labels that webd's package tables
give them. The judge's table (`tools/webkit/attackjudge.py`) is the same for both browsers but one row: the
network process may write the Downloads folder (the probe `write_downloads`).

## Not verified

* The numbers of the in-process browser (a 108 MiB program, 17 navigations in 1 to 262 ms) belong to the old
  design. The numbers of the client are in [`webd-plan.md`](webd-plan.md), "P4 as built", "Measured".
* **Download progress while a download runs.** The status bar showed "0 bytes of 200.0 KB" four seconds into a
  ten-second download in the old design: the engine's `didWriteData` did not arrive before the end on a slow
  attachment. webd now counts the bytes itself (docs/webd-plan.md, P2 slice 3); the slow-download case was not
  rerun.
* **Notifications** were verified by `tools/webplus/notify_run.py`; a service worker's `showNotification`, the
  page's `icon` and `image`, actions, sound and a system-wide list of sites are not done.
* Not tried: HiDPI, IME, the pasteboard, Settings > Default apps by a session (the preference is covered by
  `deskmenu` host tests and `mimed`'s boot self-test), a real PC.
* The harness's stand-in sites are `tools/webtest/sites.py`'s: nothing was loaded from the internet.

## `os.lazy.webkit` is retired

LazyWeb+ was the same engine with a fuller window than W5's bare browser, and its harness covers what W5's
`app_run.py` checked, so the W5 browser was removed (webd plan P1.0, [`webd-plan.md`](webd-plan.md) 9.1): the
package `os.lazy.webkit`, `LAZYOS_WEBKIT`, `run_demo.py --webkit`, the launcher's WebKit group,
`tools/webkit/app_run.py` with its judge and session. The engine processes, `webkit-smoke` and
`LAZYOS_WEBKIT_PORT` stay.
