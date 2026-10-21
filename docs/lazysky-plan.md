# LazySky: a Bluesky client for LazyOS (and Windows)

**Status:** revision 4 (2026-10-11). P0 to P8 (§9) are implemented and
verified: host tests, the paint bench, and the QEMU harness
(`python tools/lazysky/run.py`, passing on WHPX with and without
`LAZYOS_LABEL_TRACE=1`). Not done: P9 (stay signed in with `keyd`, optional)
and the Windows packaging step (optional); `tools/xui/build.py` has no
`--no-lazysky` (the crates are small and build with the other apps).

LazySky (`os.lazy.lazysky`, binary `xui-lazysky.elf`) is a desktop Bluesky
client on xui, ported from **redsky**
(<https://github.com/va1erian/redsky>, an egui client; reference commit
`4382341b0fcd`). It keeps redsky's features and its actor idea (the UI never
waits on the network) and rewrites the UI for xui.

**It is built as a headless core plus a thin UI, so the same app runs as a
LazyOS window and as a Windows/Linux/macOS desktop window** (the way
LazyGolf and the PDF Viewer are split). §2 defines the seam; every later
section respects it.

Read §11 (rules) before writing code.

---

## 1. Goals

1. **Jank-free, always.** Scrolling, clicking and typing never stall, whatever
   the network, the image sizes or the feed length. §5 makes this measurable.
2. **Redsky's features** (phases in §9):
   * Log in with a handle and an **app password**.
   * Timeline, own profile (Posts / Media / Likes), Bookmarks, Notifications
     with an unread count, "Load more" paging.
   * Post view: avatar, name, handle, date, text with links/mentions
     (facets), up to 4 images, quoted posts, repost line, like/repost counts.
   * Like, unlike, repost, unrepost (optimistic); reply and new post with up
     to 4 images; delete own post.
   * Thread view, likers and reposters lists.
   * Search for accounts and posts; user profile with counts.
   * Full-size image viewer; Raw view (the post's JSON).
   * Settings: theme, maximum image height, server.
3. **Save images to disk:**
   * **Save Image As…** from an image's context menu and from the image
     viewer (`Ctrl+S`), through xui's `FileDialog::save_file`.
   * **Download all images** of an account into
     `~/Pictures/LazySky/<handle>/`, with progress and Cancel (redsky's bulk
     download).
   * The saved file is the bytes the server sent, never re-encoded.
4. **One core, several windows.** A Windows build must need only a new
   launcher and platform glue, no change to the core and none to the UI
   crate.
5. **LazyOS citizenship:** core package, build switch, `run_demo.py` flag,
   launcher control, QEMU session with serial markers, host harness with a
   mock Bluesky server under the test CA.

## 2. Architecture: core and UI are separate crates

```
xui-app/crates/
  lazysky-core/   no xui dependency. Protocol, session, HTTPS, image
                  decode, feed model, saving, settings. Builds and tests on
                  any host. Has a headless example (`sky-cli`).
  lazysky-ui/     portable xui code. `LazySkyApp::build(ui, platform)`,
                  FeedView, pages. Depends on lazysky-core and xui-core
                  only. Host-tested with offscreen snapshots. Has the
                  `desktop` example (winit) for Windows/Linux/macOS.
xui-app/src/bin/lazysky.rs   the LazyOS binary: `launch::run(...)` only.
```

Dependency direction is one way: **bin → lazysky-ui → lazysky-core**.
`lazysky-core` must never name an xui type, a LazyOS crate (`xui_app`,
`fhs`) or a Windows API. A CI-able check: `cargo tree -p lazysky-core` must
not list `xui-*`.

### 2.1 The core's public surface

The core is driven by requests and answers with events over channels, so any
front end (xui, a terminal, a test) can use it:

```rust
pub struct Core { /* threads, agents, session */ }

impl Core {
    pub fn start(platform: Arc<dyn Platform>, settings: Settings) -> (Core, Events);
    pub fn send(&self, req: Request);          // never blocks
}
pub struct Events { /* wraps mpsc::Receiver<Event> */ }
impl Events {
    pub fn try_next(&self) -> Option<Event>;   // never blocks
}
```

* `Request` and `Event` are plain enums (login, `GetTimeline{cursor}`,
  `Like{..}`, `LoadImage{key, want}`, `SaveImage{url, path}`,
  `BulkDownload{handle, dir}`, `Cancel{id}` …). Every request carries a
  `seq: u64`, echoed in its events, so a UI can drop answers for pages it has
  left.
* **Core images are `Bitmap { w, h, rgba: Vec<u8> }`**, already downscaled and
  cropped to the box the UI asked for (R4). The UI turns a `Bitmap` into
  `xui_core::Image::from_rgba`.
* **The core has no wake-up callback.** LazyOS's backend has no cross-thread
  waker, so the UI polls `try_next` from a timer. A desktop front end may use
  a wake hook if its backend has one; an optional `Core::set_notify(Fn())`
  exists for that, and is `None` on LazyOS.

### 2.2 The platform seam

Everything OS-specific the core needs sits behind one trait, defined in the
core and implemented by each front end:

```rust
pub trait Platform: Send + Sync {
    fn ca_roots(&self) -> Roots;               // PEM bundle path, bytes, or native store
    fn app_dir(&self) -> PathBuf;              // settings.toml, session.bin
    fn pictures_dir(&self) -> PathBuf;         // default folder for saves
    fn open_url(&self, url: &str) -> Result<(), String>;
    fn log(&self, line: &str);                 // serial marker on LazyOS, stdout on desktop
}
```

| | LazyOS (`bin/lazysky.rs`) | Desktop (`desktop` example) |
|---|---|---|
| `ca_roots` | `SSL_CERT_FILE`, then `fhs::etc::CA_BUNDLE`, then `fhs::etc::LINUX_CA_BUNDLE` | `SSL_CERT_FILE`, else the OS store through `rustls-native-certs` (feature `native-roots`) |
| `app_dir` | `fhs::app_data_dir(home, "os.lazy.lazysky")` | `%APPDATA%\LazySky` / `$XDG_CONFIG_HOME/lazysky` / `~/Library/Application Support/LazySky` |
| `pictures_dir` | `$HOME/Pictures`, else `xui_app::platform::dirs::default_dir()` | `%USERPROFILE%\Pictures`, `~/Pictures` |
| `open_url` | `mimed` (`os.lazy.mimed.v1`), as LazyWeb hands links off | `cmd /c start`, `xdg-open`, `open` |
| `log` | `println!` (serial) | `println!` |

`fhs` is a LazyOS crate, so the LazyOS `Platform` implementation lives in the
**bin**, not in either library crate. `native-roots` is a feature of the core
that only the desktop example turns on; LazyOS images never link it (the
licence rule in §6.2 applies to everything linked into LazyOS).

### 2.3 What belongs where

| Concern | Crate |
|---|---|
| Protocol types, XRPC, session/refresh, facets, record builders, error mapping | core `proto/` |
| HTTPS agent, TLS config, caps, retries, rate limiting | core `net/` |
| Image fetch, decode limits, cover-crop, resize, circle mask | core `images/` |
| Save Image / bulk download, filename sanitising | core `save/` |
| `Settings` (serde, TOML), `Platform`, `Core`, `Request`/`Event` | core |
| **Feed model:** rows, prefix sums, scroll anchoring, visible range, generic over a row payload `Feed<P>` | core `feed/` |
| Shaping text, `Prepared` rows, the layout thread | ui `prepare` |
| FeedView painter, hit testing, scroll bar, autoscroll, events | ui `feed/` |
| Pages, dialogs, menus, nav stack, composer, viewer, settings page | ui `app/` |
| `Bitmap` → `xui_core::Image`, decoded-image LRU | ui `images.rs` |
| `launch::run`, `Platform` for LazyOS, serial markers | bin |
| winit window, `Platform` for the desktop | ui `examples/desktop.rs` |

The feed model is generic (`Feed<P>`) so its anchoring logic is tested in the
core with `P = ()`; the UI instantiates it with `P = Prepared` holding xui
`TextLayout`s.

## 3. Non-goals (v1)

* Video playback (thumbnail and a "▶ video" badge), animated GIF playback.
* DMs, lists, custom feeds, moderation UI, translation, drafts.
* OAuth. v1 uses app passwords through `createSession`.
* Emoji and CJK glyphs: the bundled UI font has none, so they draw as tofu.
* Persisting the password (§7.4). A refresh token sealed by `keyd` is the
  optional P9.
* Redsky's self-updater, `keyring`, `rfd`, `directories`.

## 4. Redsky → LazySky mapping

| redsky | LazySky |
|---|---|
| `egui`/`eframe` immediate mode | xui retained UI; chrome from builders; the feed is one custom-painted `Control` (§6) |
| `BskyActor` on tokio, `std::sync::mpsc` | `lazysky-core`'s threads with blocking `ureq`, `mpsc` channels (§7.1). No tokio |
| `atrium-api` + `bsky-sdk` | A small hand-written XRPC layer in core `proto/` with `serde` structs for the fields used |
| `reqwest` | `ureq =3.4.2` + `rustls =0.23.45` + `nettls-crypto` (LazyWeb's stack and pins) |
| `image::load_from_memory` in `spawn_blocking` | `image 0.25` (`jpeg,png,gif,webp`; already in `xui-app/Cargo.lock`) on the core's image threads, decoding **and downscaling to display size** |
| `egui::TextureHandle` cache | LRU byte-budget cache of `xui_core::Image` (UI), modelled on `xui-app/crates/pdfview/src/cache.rs` |
| `FeedItem::Dehydrated { height }` | A `FeedRow` keeps its height forever and drops shaped text/images when far away (§6.4) |
| egui viewports (extra OS windows) | One window with a navigation stack (Back, `Alt+Left`); small forms use `Dialog` |
| `rfd::FileDialog` | xui `FileDialog::open_file` / `save_file` (model: `xui-app/crates/paint/src/view/app/dialogs.rs`) |
| `keyring` | Memory only, like Mail (`xui-app/mail/src/secrets.rs`) |
| `directories` + `settings.toml` | `Platform::app_dir()/settings.toml` |
| Middle-click autoscroll | Kept: same 10 px dead zone, 0.1 gain |

## 5. The jank budget (the contract)

Tested by the host bench and the QEMU judge (§10). A change that breaks one
is not done.

**R1. The UI thread never does:** network I/O, JSON parsing, image decode or
resize, text shaping (`TextShaper::layout`), or file I/O (settings are read
once at start, before the first frame).

**R2. Rows enter the feed fully prepared.** A row is inserted only after the
layout thread has measured it; its height is final. Text is already shaped;
image boxes come from the API's `aspectRatio` (16:9 when missing); quote
frames are measured. A late image fills its box and never changes the height,
so nothing on screen moves by itself.

**R3. Scroll anchoring.** The scroll position is `(anchor row id, offset
into that row)`, never a raw pixel offset. Changes of height or of rows above
the viewport (refresh prepending, resize re-layout, deleted post) keep the
anchor row where it is on screen.

**R4. Paint is O(visible rows).**
* Walk only rows intersecting the viewport (prefix sums + binary search).
* Every drawn image is already at display size (±1 px): `draw_image` never
  scales a large bitmap in software.
* Text uses `Canvas::draw_layout` from cached layouts; the painter never calls
  `draw_text` with wrapping and never `measure_text`.

**R5. Bounded work per tick.** The UI timer drains core events for at most
**4 ms**, then yields; at most **6** new images become `xui_core::Image` per
tick. Leftovers wait for the next tick.

**R6. Stale work is cancelled.** Image and layout queues are rebuilt on each
scroll, nearest to the viewport first (pdfview `Pool::set_jobs` semantics: the
new list replaces what still waits). A fetch whose row is more than 3 screens
away is dropped before it starts.

**R7. Budgets** (1280×720, scale 1, release):

| Measure | TCG | WHPX/KVM | Host |
|---|---|---|---|
| FeedView paint p99 | – | ≤ 8 ms | ≤ 4 ms |
| FeedView paint max | ≤ 25 ms | – | – |
| UI tick work max | ≤ 4 ms | ≤ 4 ms | ≤ 4 ms |

**R8. Adaptive timer.** 16 ms while anything is pending (requests in flight,
queued events, autoscroll, smooth scroll); 250 ms when idle.

## 6. FeedView (UI crate)

One custom `Control` (`NodeKind::Custom`) draws every list of posts: timeline,
profile tabs, bookmarks, thread, search results, notifications. Model it on
`xui-app/crates/pdfview/src/view.rs` (`PageView`): painter closure over
`Rc<RefCell<..>>` state, `on_events`, its own scroll bar and wheel step
(`WHEEL_96 = 48`, `BAR_96 = 10`), a timer. Do **not** use `scroll(layout)`:
`ScrollView` measures all its content and is not virtualized.

### 6.1 Data

```rust
// core::feed — generic, no xui
pub struct FeedRow<P> {
    pub id: RowId,           // stable: hash of post URI (+ repost reason)
    pub height: i32,         // device px, final once inserted (R2)
    pub kind: RowKind,       // Post(Arc<Post>) | Notification(..) | LoadMore{cursor}
    pub prepared: Option<P>, // None while dehydrated
}
pub struct Feed<P> { rows: Vec<FeedRow<P>>, tops: Vec<i32>, anchor: (RowId, i32), width: i32 }

// lazysky-ui — what the layout thread produces for a row at a width
pub struct Prepared {
    pub width: i32,
    pub name: Box<dyn TextLayout>,   // display name, bold, one line
    pub meta: Box<dyn TextLayout>,   // "@handle · 3h"
    pub body: Box<dyn TextLayout>,   // wrapped text
    pub links: Vec<LinkSpan>,        // facet hit boxes, relative to the row
    pub images: Vec<ImageBox>,       // url + rect (from aspectRatio)
    pub quote: Option<Box<Prepared>>,
    pub buttons: ButtonRects,        // like / repost / reply / "…"
}
```

`TextShaper` and `TextLayout` are `Send + Sync`. Take the shaper once on the
UI thread (`ui.text_shaper()`) and hand it to the layout thread.
`Canvas::draw_layout` draws only layouts shaped by this backend; this shaper
is the backend's own. **The spike must prove this off the UI thread under the
LazyOS backend** (§9, P0) before anything depends on it.

### 6.2 Row layout (design px at 96 dpi; scale with `xui_app::hidpi` on LazyOS, the backend's scale on desktop)

```
┌───────────────────────────────────────────────────────────┐
│ ↻ reposted by Alice                    (reposts only)      │
│ ┌────┐ Display Name                                        │
│ │48px│ @handle.bsky.social · 3h                            │
│ └────┘                                                     │
│ Post text wrapped at row width − 16, links/mentions in the │
│ link colour.                                               │
│ ┌──────────┐┌──────────┐  1 image: full width, ≤ max_h     │
│ │  image   ││  image   │  2: two columns; 3–4: 2×2         │
│ └──────────┘└──────────┘                                   │
│ ┌ quoted post (frame, 8 px inset, no images) ─────────────┐│
│ ♡ 42   ↻ 7   Reply   …                                     │
├───────────────────────────────────────────────────────────┤
```

Images are cover-cropped into their box on the worker. Relative dates ("3h")
are shaped at prepare time, never at paint.

### 6.3 Events

| Input | Action |
|---|---|
| Wheel | `anchor_offset += notches * WHEEL_96 * scale`, normalise the anchor |
| `PageUp/PageDown/Home/End/Space` | Page / ends |
| Scroll-bar drag | Scroll |
| Middle click | Autoscroll |
| Link span | `Platform::open_url` |
| Mention | Push a Profile page |
| Image click | Push the Image viewer |
| Image right-click | Menu: **Save Image As…**, **Open Image**, **Copy Image Address** |
| ♡ / ↻ | Optimistic like/repost (redsky's `update_post_optimistically`) |
| Reply | Open the composer |
| "…" | **Delete Post** (own only), **Raw View**, **Copy Link to Post** |
| Elsewhere in a row | Push the Thread page |

Hit testing uses only `Prepared` rects. Menus: `Menu::context(ui)`.

### 6.4 Dehydration

A row more than **3 viewport heights** away drops `prepared` and keeps
`height`; its images become evictable. It is re-prepared (nearest first) when
within **2 viewport heights**. If a row reaches the viewport before its
`Prepared` arrives (rare), paint a flat skeleton at its known height (grey
bars, no animation). A width change re-prepares visible rows first; stale
layouts are drawn clipped until the new one arrives, and heights update
through R3.

## 7. Core internals

### 7.1 Threads (all in the core; the UI owns only its own thread and the layout thread)

| Thread | Owns | Does |
|---|---|---|
| UI (front end) | xui `Ui`, `Feed<Prepared>`, image LRU | Sends `Request`s; drains `Events` within R5; paints |
| `api` (1) | One `ureq::Agent`, the `Session` | XRPC calls in order, token refresh, JSON → `Post` |
| `img` (4) | One `ureq::Agent` each | Nearest job first: fetch, decode, crop, resize → `Bitmap` |
| `save` (on demand) | Its own `ureq::Agent` | Save Image / bulk download; `*.part` then rename |
| `layout` (UI crate, 1) | The `TextShaper` | `Post` + width → `Prepared` |

**Socket rule.** On LazyOS each thread has its own descriptor table
(`docs/tls-plan.md` §4.2), so a socket must stay on the thread that opened it.
Never share an `Agent` across threads and never move a connection.

**Provider.** The binary installs
`rustls::crypto::CryptoProvider::install_default(nettls_crypto::provider())`
before any thread starts (Mail's `main.rs` does the same). The desktop
example does the same.

### 7.2 HTTP/TLS

`tls.rs`, `roots.rs` and `resolve.rs` (now in the shared `lazyhttps` crate,
`xui-app/crates/https`, which LazyAgent uses too; docs/ai-agent-plan.md) are
small adaptations of the fetcher of LazyWeb (the Blitz browser, since removed)
(`xui-app/web/src/fetch/tls.rs` and `roots.rs`: rustls client config with
`nettls_crypto::provider_arc()`, ALPN `http/1.1`, a ureq `Connector`; roots
from `Platform::ca_roots()`). Do not depend on the `lazyweb` crate (declared
GPL-2.0-only); copy, name the source in a comment, keep the two in step.

* `User-Agent: LazySky/<version>`; 10 s connect, 30 s read.
* Body caps: JSON 8 MiB, images 16 MiB.
* No pooling across threads (socket rule).

### 7.3 XRPC (`proto/`)

All calls are HTTPS to `<pds>/xrpc/<nsid>` with `Authorization: Bearer
<accessJwt>`; the PDS proxies `app.bsky.*` reads to the AppView.

**Login:**
1. `POST {service}/xrpc/com.atproto.server.createSession`
   `{"identifier":handle,"password":app_password}`. Default service
   `https://bsky.social`; Settings can change it (the harness points it at a
   mock).
2. Response: `accessJwt`, `refreshJwt`, `did`, `handle`, `didDoc`. The PDS is
   the `didDoc.service[]` entry whose `id` ends `#atproto_pds`
   (`serviceEndpoint`); keep `service` if there is none.
3. `400/401` with `error` `ExpiredToken`/`InvalidToken`: call
   `com.atproto.server.refreshSession` (Bearer `refreshJwt`), retry **once**.
4. `429`: wait until `ratelimit-reset` (cap 60 s); emit a status event.

| Feature | NSID |
|---|---|
| Timeline | `app.bsky.feed.getTimeline?limit=30&cursor=` |
| Profile posts / media | `app.bsky.feed.getAuthorFeed?actor=&filter=posts_no_replies` / `posts_with_media` |
| Profile likes | `app.bsky.feed.getActorLikes` |
| Bookmarks | `app.bsky.bookmark.getBookmarks` |
| Thread | `app.bsky.feed.getPostThread?uri=&depth=6` |
| Likers / reposters | `app.bsky.feed.getLikes`, `getRepostedBy` |
| Profile | `app.bsky.actor.getProfile` |
| Search | `app.bsky.actor.searchActors`, `app.bsky.feed.searchPosts` |
| Notifications | `app.bsky.notification.listNotifications`, `getUnreadCount`, `updateSeen` |
| Like / repost / post | `com.atproto.repo.createRecord` (`app.bsky.feed.like`, `.repost`, `.post`) |
| Unlike / unrepost / delete | `com.atproto.repo.deleteRecord` (rkey = last URI segment) |
| Image upload | `com.atproto.repo.uploadBlob` (raw body; type sniffed from magic bytes) |

Embeds parse into `enum Embed { Images, External, Record, RecordWithMedia,
Video, Unknown }`; an unknown `$type` is `Unknown` and never fails a page.
`reasonRepost` gives the "reposted by" line.

**Facets** use **UTF-8 byte offsets**. Validate before use: `start <= end <=
len` and both on `char` boundaries; drop a facet that fails.

**New post record:** `{"$type":"app.bsky.feed.post","text","createdAt"
(RFC 3339),"langs":["en"]}` plus optional `reply:{root,parent}` (both
`{uri,cid}`) and `embed` (`app.bsky.embed.images` with uploaded blobs, `alt`,
`aspectRatio`). Composer limit 300 graphemes (count `chars` as an
approximation, say so in a comment).

**Raw view:** keep each post's `serde_json::Value` until dehydration;
pretty-print once on the api thread when asked (redsky's `.Jules/bolt.md`
lesson: never format JSON in the render path).

### 7.4 Credentials

The password goes to the api thread once; only the session JWTs stay, in
memory. Never print a password or JWT (the harness greps the serial log). The
handle may be stored in `settings.toml` ("Remember handle"). P9 optionally
seals the `refreshJwt` with `os.lazy.keyd.v1.Wrap` (LazyOS only; the desktop
build leaves it off).

### 7.5 Images

* The feed asks for `thumb` (avatars `avatar_thumbnail`); the viewer and Save
  use `fullsize`. URLs are used as the API gives them.
* Cache key `(url, box_w, box_h)`; 64 MiB of decoded RGBA, LRU, visible
  entries never evicted.
* Avatars are circle-masked on the worker, so paint is one `draw_image`.
* A decode failure shows a grey broken-image box; no retry until refresh.
* Hostile input: refuse over 8192×8192 or 64 Mpx before allocating
  (`image::ImageReader` with `Limits`).

### 7.6 Saving images (core `save/`, driven by the UI's dialog)

1. **Save Image As…** → `FileDialog::save_file`. Suggested name
   `<handle>_<rkey>_<n>.<ext>`; `<ext>` from the URL suffix (`@jpeg`→`jpg`,
   `@png`, `@webp`, `@gif`, else `jpg`). Start folder
   `Platform::pictures_dir()`, created if missing.
2. On accept the UI sends `Request::SaveImage { url: fullsize, path }`. The
   save thread fetches, writes `<path>.part`, flushes, renames. Events:
   `SaveDone{path, bytes}` / `SaveFailed{reason}`; markers
   `SKY:SAVE:DONE:<path>:<bytes>` / `SKY:SAVE:FAIL:<reason>`.
3. The dialog's overwrite prompt is the only confirmation.
4. **Download all images** (Profile "…" menu): scan `getAuthorFeed?filter=
   posts_with_media&limit=100` through the cursor, 100 ms between pages;
   only the account's own non-reply posts; files into
   `<pictures>/LazySky/<sanitized handle>/` named
   `img<date>_<cid prefix>.<ext>` (`:` → `-`); 3 concurrent; progress events
   ("Scanning… 240 posts", "Downloading 12/57"); Cancel sets an
   `AtomicBool` checked between files; ends with `SKY:BULK:DONE:<n>:<errors>`.
5. **Sanitising:** keep `[A-Za-z0-9._-]`, replace the rest with `_`, no
   leading `.`, truncate to 100 bytes. Same shape as LazyWeb's
   `downloads.rs` `sanitize()`; copy its tests.

## 8. Code layout

```
xui-app/crates/lazysky-core/
  Cargo.toml  (features: native-roots)
  src/lib.rs           Core, Request, Event, Events, Settings, Platform, Bitmap
  src/proto/           session.rs feed.rs embed.rs facets.rs record.rs error.rs
  src/net/             tls.rs roots.rs http.rs api.rs
  src/images/          pool.rs decode.rs crop.rs
  src/save/            save.rs bulk.rs sanitize.rs
  src/feed/            model.rs (Feed<P>, anchoring, prefix sums)
  examples/sky_cli.rs  log in, print the timeline, save the first image
  tests/               proto_fixtures.rs https.rs feed_model.rs images.rs
  tests/fixtures/*.json
xui-app/crates/lazysky-ui/
  Cargo.toml  (feature: desktop = winit backend + core/native-roots)
  src/lib.rs           LazySkyApp, Msg, WINDOW
  src/prepare.rs       layout thread, Prepared
  src/feed/            view.rs paint.rs events.rs cache.rs
  src/app/             mod.rs nav.rs login.rs composer.rs profile.rs thread.rs
                       notifications.rs search.rs viewer.rs settings.rs
                       status.rs perf.rs
  examples/desktop.rs  the Windows/Linux/macOS window
  tests/window.rs      offscreen snapshots (light and dark)
xui-app/src/bin/lazysky.rs            LazyOS launcher + Platform
xui-app/packages/lazysky/{manifest.toml, docs/README.md, icons/}
build_support/lazysky_embed.rs  build_support/tests/lazysky_tests.rs
tools/lazysky/{run.py, mockserver.py, gen_fixtures.py, judge.py,
               test_judge.py, session.py, README.md}
tools/screenshot/examples/lazysky.json     (generated by session.py --write)
docs/lazysky.md                            (user/developer doc, P8)
```

Both crates are `xui-app` workspace members (like `golf`, `pdfview`).
Dependencies:

* **core:** `serde`, `serde_json`, `toml`, `ureq =3.4.2` (no default
  features), `rustls =0.23.45` (`std`, `tls12`, no default features),
  `rustls-pki-types`, `nettls-crypto` (path `../../../nettls/crypto`),
  `image 0.25` (`default-features = false`, `jpeg,png,gif,webp`), `flate2`
  (`rust_backend`) only if a server gzips; optional `rustls-native-certs`
  behind `native-roots`.
* **ui:** `xui-core` at the workspace's pinned rev, `lazysky-core`; dev:
  `xui-canvas`; feature `desktop` adds `xui-canvas` with `winit-backend`.
* **Never add:** `tokio`, `reqwest`, `ring`, `aws-lc-rs`, `atrium-*`, `egui`.
  `rustls-native-certs` is allowed only behind `native-roots`.

The bin is built by the existing `xui-app` build (a `[[bin]]` plus
`tools/xui/build.py`'s `BINS`, as `golf` is).

## 9. Phases

Each phase is one change and ends with `cargo fmt`, clippy `-D warnings`, the
host tests, and every file under 500 lines (`python tools/check_file_length.py`).

**P0: spike and skeleton (this is what starts next).**
1. Create `lazysky-core` and `lazysky-ui`, add them to the `xui-app`
   workspace, and the thin `src/bin/lazysky.rs` that prints `SKY:UP:PASS`.
2. Core: `Platform`, `Roots`, and `net/{tls,roots,http}.rs`; a host test
   `tests/https.rs` that fetches from a local rustls server with an `rcgen`
   CA (copy `xui-app/web/tests/support/mod.rs` and `fetch_tls.rs`).
3. Core: `examples/sky_cli.rs` stub that does one HTTPS GET through the
   `Core` API (proves the headless path).
4. UI spike (a throwaway test in `lazysky-ui/tests/`): take
   `OffscreenBackend::new().text_shaper()` (fonts registered as in
   `crates/settings/tests/window.rs`), shape a paragraph **on another
   thread**, send the `Box<dyn TextLayout>` back, draw it with
   `Canvas::draw_layout` in a `xui_canvas::snapshot::render_with` snapshot,
   and Read the PNG.
5. LazyOS check of the same risk: in the bin, behind a `--spike` argument,
   shape on a thread through the real backend's `text_shaper()` and draw it;
   run it in QEMU (`LAZYOS_XUI_AUTOSTART`, serial `SKY:SPIKE:SHAPE:PASS`).
   If `draw_layout` refuses it, the feed falls back to shaping on the UI
   thread inside the R5 budget; record the finding in this document.
6. `desktop` example opens an empty window on Windows.

*Done when:* the tests pass; `cargo tree -p lazysky-core` lists no `xui-*`;
the musl build of the bin succeeds; the desktop example opens on Windows; the
QEMU spike prints its marker (or the fallback decision is written down).

*P0 findings (2026-10-11).*
* **Off-thread shaping works.** A `Box<dyn TextLayout>` shaped on a worker
  thread from `ui.text_shaper()` draws correctly with `Canvas::draw_layout` on
  the offscreen backend (`lazysky-ui/tests/window.rs`, light and dark
  snapshots) and under xui's winit backend on Windows (`desktop` example). The
  LazyOS backend (`SKY:SPIKE:SHAPE:PASS`) is not yet run: the bin is built
  (musl, release) but is not packaged until P8, so item 5 stays open. Until it
  is run, §6.1's fallback (shape on the UI thread inside R5) is the plan B.
* **The offscreen backend has no timers.** A view that polls a worker from a
  timer returns `Msg::Poll`; a test emits it itself after waiting for the
  worker (`stage.emit(Msg::Poll)`).
* **xui pin.** The workspace is at rev `93592fdc` (not `e50e69d` of §12).
  Every new crate uses the workspace's rev.
* **No `rustls` in the bin.** `lazysky_core::install_tls_provider()` installs
  `nettls-crypto`, so the root `xui-app` package gains no TLS dependency.
* **The core's TLS config names its provider explicitly**, so the core works
  without a process-wide default provider.
* `sky_cli --features native-roots` fetched `https://bsky.social/xrpc/_health`
  through `Core` (HTTP 200).

**P1: protocol.** `proto/` with every type in §7.3, `#[serde(default)]` on
optional fields, facet validation, record builders, error mapping; fixtures
(timeline with reposts, quotes, 1–4 images, external link, video, unknown
embed; a thread; notifications; a createSession with `didDoc`). *Done when:*
`cargo test -p lazysky-core proto` passes, including bad facets, a missing
`aspectRatio` and an unknown `$type`.

**P2: api thread and login.** `net/api.rs`: session, PDS from `didDoc`,
refresh-and-retry, 429; `Core`/`Events`; `sky_cli` logs in and prints a
timeline; the UI login page and the tick drain (R5, R8); `tools/lazysky/
mockserver.py` (Python `ssl` server using the CA from
`tools/net/tlscerts.py`, serving fixtures and generated pages, logging each
request as JSON). *Done when:* a host test logs in against an in-process
server and `SKY:LOGIN:PASS:<handle>` prints.

**P3: FeedView, text only.** `feed/model.rs` (rows, prefix sums, R3
anchoring, visible-range search) fully unit-tested in the core (prepend,
resize, delete); UI `prepare`, `paint`, `events`, `view`; the Timeline tab
with Load more; `app/perf.rs` (§10.3). *Done when:* offscreen snapshots of a
300-post feed in both themes (Read them) and an `#[ignore]` release bench
paints 1,000 frames of a 2,000-row feed with p99 ≤ 4 ms.

*P3 findings (2026-10-11).*
* **A pushed clip makes every later draw expensive, and the canvas is not
  clipped to the widget's bounds for you.** With any clip active each
  operation applies a mask: five rows cost 6 ms a frame with a clip per row
  and 0.7 ms with none, but without any clip a row scrolled partly above the
  view paints over the header. FeedView clips only the rows that stick out of
  the view (the first and the last) and rows whose layout is for another
  width, and draws every other row bare (bench: p50 2.4 ms, p99 2.9 ms over
  1,000 frames of a 2,000-row feed, release, host, against the 4 ms budget).
  Any new painter code follows this; the bench catches a regression.
* **Link runs are tinted, not recoloured.** `Canvas::draw_layout` draws one
  colour, so facet ranges get an accent-tinted box behind the text
  (`TextLayout::selection_rects`) and a hit region each.
* **The row model found a real bug in its own random walk** (a removed anchor
  row was reset to the first row before the "move to the next" rule ran);
  `feed/tests.rs` keeps the walk, with the invariants checked after every
  operation.
* Heights are final at insertion: a page is staged until the layout thread
  has shaped every row (`timeline::Staging`), then inserted at once.

**P4: images.** Core image pool (nearest-first, queue replaced on scroll,
stale drop, decode limits, cover-crop, circle avatars), UI cache and
dehydration (§6.4). *Done when:* tests cover crop sizes, circle mask, a
decode-bomb refusal, LRU never evicting visible entries, queue replacement;
the bench still holds with images.

*P4 findings (2026-10-11).*
* Pictures are fetched, decoded under limits (8192 px a side, 64 Mpx, read
  from the header before any allocation), cover-cropped to the box and, for
  avatars, masked to a circle on four image threads; the UI turns each into
  an `xui_core::Image` at most six per tick and draws it with one
  `draw_image`. `Core::want_images` replaces the waiting list (R6): a picture
  that waited and is no longer wanted is never fetched
  (`tests/images.rs::a_new_list_drops_what_waited...`).
* The decoded-picture cache is a 64 MiB LRU that never evicts what the view
  shows; failures are remembered until a refresh.
* The UI's layout thread and the core's image threads each build their own
  `Http` on their own thread, as the socket rule requires.

**P5: actions.** Like/unlike/repost/unrepost (optimistic, rolled back with a
status error), composer (up to 4 images via `FileDialog::open_file`, reply),
delete own post, thread page, likers/reposters (`list()` `ListView`, fixed
rows), Raw view. Markers `SKY:LIKE:<ok|fail>`, `SKY:POST:PASS:<uri>`.

*P5 findings (2026-10-11).*
* The core exposes likes, reposts, deletes (own records only, checked
  client-side), posts with up to four pictures (read and uploaded on the api
  thread; refused above Bluesky's 1 MB), threads, people lists and the Raw
  view. `SKY:LIKE:ok|fail`, `SKY:REPOST:ok|fail`, `SKY:POST:PASS:<uri>` come
  from the core through `Platform::log`.
* The window is a stack of pages over the timeline (`Nav`): timeline, thread,
  people, raw, composer; Back and Alt+Left pop. A row opens its thread; the
  "..." menu has Raw view, Copy link, Liked by, Reposted by, Delete (own
  posts only).
* Likes and reposts are optimistic: the post is patched at once (its old
  layout is drawn until the new one arrives, `FeedRow::dirty`), confirmed
  with the record URI the server returns, and rolled back with a status
  line when it refuses (`actions.rs`, `tests/interact.rs`).
* The composer keeps its text from the field's change events (the model
  holds it, not the widget), which also makes it testable headlessly.

**P6: profile, bookmarks, notifications, search, settings.** Nav stack with
Back and `Alt+Left`; Profile (Posts/Media/Likes, counts); Bookmarks;
Notifications (rows in FeedView, unread count in the tab, `updateSeen` on
open, `getUnreadCount` polled every 60 s from the api thread); account and
post search; Settings (theme, max image height, server, remember handle).

**P7: saving images.** The image viewer (fit to window, `Ctrl+=`/`Ctrl+-`/`0`
zoom with the resize on a worker, drag to pan, `Ctrl+S`); Save Image As… and
bulk download exactly as §7.6, with sanitize tests.

*P6 findings (2026-10-11).*
* Profile (counts, Posts / Media / Likes tabs), bookmarks, search (accounts or
  posts; a `#tag` goes to posts) and thread pages share the detail pane;
  people lists and notifications share the people page. **Deviation from
  §9:** notifications are a list (one line each, a reply's text after it),
  not FeedView rows: a like or a follow has no post to lay out. Opening one
  goes to its subject's thread, a follow to the profile. The unread count is
  polled every 60 s from the tick and shown on the button; opening the list
  marks everything seen.
* Settings live in `settings.toml` (`lazysky_core::Settings`): theme, tallest
  picture, server, remember handle. A bad file gives defaults, a bad number
  is clamped. They are written by the api thread (`Request::SaveSettings`),
  never the UI's. Changing the picture height marks every row dirty so the
  layout thread re-lays them out under R3.

*P7 findings (2026-10-11).*
* Save Image As, the picture menu (right click: Save, Open, Copy address)
  and the viewer (fit, zoom by asking the core for a bigger `Shape::Fit`
  bitmap, drag to pan, Ctrl+S, Left/Right for the post's other pictures).
  Saved files are the server's bytes, written through `<name>.part`.
* Download all images scans `getAuthorFeed?filter=posts_with_media` a page of
  100 at a time from the api thread (a request that arrives meanwhile goes
  first), 100 ms between pages, hands each picture of the account's own
  non-reply, non-repost posts to three save threads, skips files already
  there (so a second run is a no-op) and ends with
  `SKY:BULK:DONE:<n>:<errors>`; Cancel stops it.

**P8: LazyOS integration.** Copy the route LazyWeb took (it is gone; `build_support/lazysky_embed.rs` and
AGENTS.md are the copy now):
* `LAZYOS_LAZYSKY=1`: `build_support/lazysky_embed.rs` refuses it without
  `LAZYOS_DESKTOP=1` and `LAZYOS_NETD=1` (the CA bundle is in every image);
  add to the requested table in `xui_embed.rs` and `core_packages.rs`; tests
  in `build_support/tests/lazysky_tests.rs`.
* `tools/xui/build.py` builds `target/xui/xui-lazysky.elf` (`--no-lazysky`);
  `tools/xui/core_packages.py` entry with `optional=True`.
* `tools/demo_args.py` `--lazysky` implies `--desktop` and `--net`; wire
  `tools/demo_builds.py` and `tools/run_demo.py`.
* GUI launcher: Simple-tab checkbox "LazySky (Bluesky)", Advanced raw switch,
  `tools/lazygui/catalog.py` `build_env`/`build_plan`, tests in
  `tools/lazygui/test_catalog_apps.py`.
* Package `xui-app/packages/lazysky/manifest.toml`: `[app] name = "LazySky"`,
  `system_name = "os.lazy.lazysky"`, `category = "internet"`; `[entry]`
  `args = ["--client"]`, `abi = "linux"`; `[permissions]` `interfaces =
  ["os.lazy.display.v1","os.lazy.input.v1","os.lazy.mimed.v1"]`, `network =
  ["outbound"]`, `files = ["write:$HOME/.apps/os.lazy.lazysky",
  "write:$HOME/Pictures", "read:$HOME"]` (consent only today,
  `docs/packages.md`). **Derive the final list** from a
  `LAZYOS_LABEL_TRACE=1` session: zero `LABEL:DENY`.
* Icon (`xui-app/crates/app-icons` `PACKAGES`), `docs/lazysky.md`, package
  README, `docs/build-switches.md` row, an AGENTS.md section like the other apps',
  and `python tools/nettls/licenses.py` over the tree (no `ring`/`aws-lc`).

*P8 findings (2026-10-11).*
* LazySky runs on LazyOS under QEMU: sign-in over TLS against the mock with
  the harness CA, the timeline with decoded JPEG pictures and avatars,
  scrolling, an unread count, a thread by click. The first QEMU run printed
  `SKY:UP:PASS` from a bin with the P0 spike; the full harness (WHPX) reports
  six `SKY:PERF` lines inside R7 and the mock saw the sign-in body, a bearer
  token on every XRPC call, SNI `bsky.test` and `cdn.bsky.test` and the
  pictures. The host-side opt-in bench (p99 2.9 ms) and these numbers agree.
* `init.Launch` hands an app **one absolute path at most**, so the harness
  cannot pass `--service`; it writes `settings.toml` into the app's folder
  first and launches only once the file reads back (so a typed password can
  never reach the real service). `xui-lazysky` therefore loads
  `settings.toml` at start; `--service` still overrides it.
* BusyBox `mkdir -p ~/.apps/x` was answered "can't create directory '/home/':
  Permission denied" in the guest although `/home/user` exists, so the
  session makes one level at a time. Not investigated; a kernel look at
  `mkdir` of an existing directory (it should be `EEXIST`) is worth it.
* The package's file rules take `$HOME/<dir>`, not the home itself, so
  the composer's picker is allowed `read:$HOME/Pictures` and
  `read:$HOME/Downloads`.
* Permissions: window, cursor, input, `mimed` and `clipboard`; `network =
  ["outbound"]`.

**P9 (optional): stay signed in** with `keyd` (§7.4), LazyOS only.

**Windows packaging (after P7, optional):** `cargo build --release -p
lazysky-ui --example desktop --features desktop`; an icon and a `winres`
manifest if a distributable `.exe` is wanted. No core or UI change should be
needed; if one is, the seam in §2 is wrong and should be fixed there.

## 10. Verification

### 10.1 Host (every phase)

```bash
cargo test --manifest-path xui-app/Cargo.toml -p lazysky-core -p lazysky-ui
cargo test --manifest-path xui-app/Cargo.toml -p lazysky-ui --release bench -- --ignored --nocapture
cargo tree --manifest-path xui-app/Cargo.toml -p lazysky-core | grep -c xui   # must be 0
cargo run --release --manifest-path xui-app/Cargo.toml -p lazysky-ui --example desktop --features desktop
```

Offscreen window tests use `xui_canvas::snapshot::render_with` (model:
`crates/settings/tests/window.rs`: fonts registered, watchdog thread). They
render login, a feed with images, a thread, the composer, the image viewer
and settings, light and dark, into `xui-app/target/snapshots/lazysky-*.png`.
**Read the PNGs.** The `xui-app` lib/bin tests are Linux-only; on Windows run
those in WSL (`.claude/skills/xui-app/SKILL.md`).

### 10.2 QEMU harness: `python tools/lazysky/run.py`

Modelled on the LazyWeb harness (since removed) and `tools/mail/run.py`:
1. `gen_fixtures.py` (deterministic): 400 posts, 0–4 images with varied or
   missing `aspectRatio`, quotes, reposts, facets, PNG/JPEG thumbs and
   fullsizes (`tools/webtest/imgenc.py`, `jpegenc.py`), and one 6000×4000 JPEG to
   prove downscaling stays off the UI thread.
2. Build with `LAZYOS_DESKTOP=1 LAZYOS_NETD=1 LAZYOS_NETD_ARGS=demo=0
   LAZYOS_LAZYSKY=1 LAZYOS_UI_PROBE=1 LAZYOS_XUI_AUTOSTART=term
   LAZYOS_RESET_OS=1 LAZYOS_TLS_TEST_CA=<ca>
   LAZYOS_TLS_TEST_HOSTS="10.0.2.2 bsky.test cdn.bsky.test"`.
3. Serve `https://bsky.test` (XRPC) and `https://cdn.bsky.test` (images);
   start LazySky with `--service https://bsky.test` through `rhai`
   `init.Launch` (as `mail_tls.json` does).
4. Run `tools/screenshot/examples/lazysky.json`: log in, scroll 150 notches
   down and 50 up, open a thread and go Back, like a post, open an image,
   `Ctrl+S` to `~/Pictures/test.jpg`, Download all images on one profile,
   open Notifications; a screenshot per step.
5. `judge.py` (with `test_judge.py` proving it fails when it should) checks:
   markers `SKY:UP:PASS`, `SKY:LOGIN:PASS`, `SKY:FEED:ROWS:<n>`,
   `SKY:LIKE:ok`, `SKY:SAVE:DONE:/home/user/Pictures/test.jpg:<bytes>`,
   `SKY:BULK:DONE:<n>:0`; that `<bytes>` and the SHA-256 of the file read
   back from the OS volume (`cargo run -q -p ext2fs --example osread --
   target/lazyos.img cat ...`) match the served file; the mock's request log
   (createSession body, like `createRecord` body, a Bearer header on every
   XRPC call, SNI `cdn.bsky.test`); that no password or JWT is in the serial
   log; R7 from `SKY:PERF`; screenshots with `pngstats.py --min-nonblack
   0.05`. `--label-trace` lists `LABEL:DENY`. A manual `--live` mode against
   bsky.social takes the password through `{"type_secret":
   "LAZYSKY_PASSWORD"}`.

### 10.3 Frame-time markers (UI `app/perf.rs`)

The FeedView painter and the tick drain time themselves with `Instant`.
Every 2 s while the feed has scrolled it prints
`SKY:PERF:paint_p50=<ms>:paint_p99=<ms>:paint_max=<ms>:tick_max=<ms>:frames=<n>`,
and `SKY:PERF:TOTAL:...` on close. The judge fails the run on any R7 breach.
(The UI reports through `Platform::log`, so the desktop build prints the
same lines.)

## 11. Rules for the implementing agent

1. **Read first:** AGENTS.md (Project conventions; Packages and the
   label-policy trace; LazyWeb; Mail; LazyGolf for the portable-crate +
   desktop-example pattern); `.claude/skills/xui-app/SKILL.md`; xui's
   `docs/cookbook.md` and `docs/widgets.md` in the cargo checkout of the
   pinned rev (`~/.cargo/git/checkouts/xui-*/93592fd/`);
   `xui-app/crates/pdfview/src/{view.rs,worker.rs,cache.rs}`;
   `xui-app/web/src/fetch/{tls.rs,roots.rs,pool.rs}`.
2. **Use redsky for behaviour, not code.** It is egui and async.
3. **Respect the seam (§2).** No xui type, `fhs` path or OS call in
   `lazysky-core`. No LazyOS-only call in `lazysky-ui` (route through
   `Platform`). Every new OS need becomes a `Platform` method with both
   implementations in the same change.
4. **Respect the jank budget (§5).** If a rule seems impossible, stop and say
   so; do not weaken it. Never call a blocking `recv`, `TextShaper::layout`,
   `image::*` or a file write from UI-thread code.
5. **Files under 500 lines**; keep `proto` and `feed/model.rs` free of I/O.
6. **Validate untrusted input:** every server field, facet offset and image
   dimension. `unwrap()` on server data is a bug. No `unsafe`.
7. **Paths** come from `libs/fhs` on LazyOS only (in the bin);
   `python tools/fhs/check_literals.py`.
8. **Don't commit generated output** (`shots/`, snapshots, fixture images);
   commit `gen_fixtures.py` instead.
9. **Never print secrets** (password, JWTs) to stdout, logs or test output.
10. **Keep the xui pin.** Don't bump the rev; if xui needs a change, stop and
    describe it.
11. **Report each phase** with the commands run, a summary of their output,
    and light and dark screenshots.

## 12. Known facts from the survey (so nobody re-derives them)

* xui rev `93592fdc7540e3f3ba21cd01f05ed1d949c3b3f6` (the workspace's pin; the
  first draft said `e50e69d`), every crate must use the same one; local
  checkout under `~/.cargo/git/checkouts/xui-e7bf80847fe0275b/93592fd/`.
* `xui_core::Image::decode` handles PNG and JPEG only; GIF/WebP come from the
  `image` crate (already in `xui-app/Cargo.lock` via `xui-blitz`).
  `Canvas::draw_image` is bilinear and backends cache uploads by `Image::id`.
* There is no standalone image widget; images are drawn from a custom
  `Control`, a `GridView` tile painter, or tree rows.
* `ScrollView` is not virtualized; `ListView`, `IconView`, `GridView` and
  lazy `TreeView` are. Mail's lists are `ListView`s over custom models.
* The LazyOS backend's `waker()` is a no-op (`xui-app/src/backend/
  handlers.rs`): proxied messages are drained on the next tick, and a client
  parks until an input event or a due timer. Every app keeps a timer running
  (pdfview 30 ms, Mail 100 ms).
* `ui.proxy()` gives a `Send` message proxy; on LazyOS it only helps if a
  timer is also running.
* LazyWeb's fetcher spawns one thread per fetch (512 KiB stack, max 6) and
  keeps sockets on the opening thread.
* `confd` holds per-user settings (`user/<uid>/...`); LazyWeb and Mail keep
  files under `$HOME/.apps/<system_name>`. `keyd` exposes `Wrap`/`Unwrap`
  keyed per uid, payload ≤ 8 KiB; no xui app uses it yet.
* Package `files` rules are consent-only today (`docs/packages.md`).
