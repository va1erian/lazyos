# LazySky: the Bluesky client

LazySky (`os.lazy.lazysky`, binary `xui-lazysky.elf`) is a desktop Bluesky
client on xui, ported from [redsky](https://github.com/va1erian/redsky). It
runs as a LazyOS window and, from the same code, as a Windows, Linux or macOS
window. The design and its rules are in [`lazysky-plan.md`](lazysky-plan.md);
this page is how to use and build it.

## Using it

* **Sign in** with a handle and an *app password* (Bluesky: Settings, Privacy
  and security, App passwords). The password is held in memory only; the
  handle is saved only if *Remember my handle* is ticked in Settings.
* **Timeline** with *Load more*; a post shows its author, age, text with links
  and mentions, up to four pictures, a quoted post and a repost line.
  Click a link to open it, a row for its thread, a picture for the viewer.
* **Like, repost, reply, post** (up to four pictures, 300 characters);
  **Delete** your own posts from the "..." menu, which also has *Raw view*,
  *Copy link*, *Liked by* and *Reposted by*.
* **Profile** (Posts, Media, Likes, with counts), **Bookmarks**,
  **Notifications** (unread count on the button, marked seen when opened),
  **Search** (accounts or posts; `#tag` goes to posts).
* **Pictures**: the viewer fits the window (`Ctrl+=` / `Ctrl+-` / `Ctrl+0`
  zoom, drag to pan, `Ctrl+S` saves); right-click a picture for *Save image
  as...*; *Download images* on a profile saves the account's own pictures
  into `~/Pictures/LazySky/<handle>/` (a second run leaves existing files
  alone; *Cancel download* stops it).
* **Settings**: theme, the tallest picture in a post, the server, remembering
  the handle. They live in `settings.toml` in the app's folder.
* `Alt+Left` goes back; the wheel, `PageUp`/`PageDown`, `Home`/`End`, the
  scroll bar and middle-button autoscroll scroll a feed.

## Running it

```bash
python tools/run_demo.py --lazysky          # desktop + networking + LazySky
python tools/lazyos_gui.py                  # Simple tab: "LazySky (Bluesky) client"
cargo run --release --manifest-path xui-app/Cargo.toml -p lazysky-ui \
    --example desktop --features desktop     # the same window on Windows/Linux/macOS
cargo run --manifest-path xui-app/Cargo.toml -p lazysky-core --example sky_cli --features native-roots
```

`LAZYOS_LAZYSKY=1` needs `LAZYOS_DESKTOP=1` and `LAZYOS_NETD=1`; the build
refuses it otherwise (`build_support/lazysky_embed.rs`). `--service URL`
(a `xui-lazysky` argument) signs in against another Bluesky server.

## How it is built

```text
xui-app/crates/lazysky-core   no xui, no LazyOS: protocol, session, HTTPS,
                              images, saving, settings, the feed model
xui-app/crates/lazysky-ui     the portable xui window (FeedView, pages)
xui-app/src/bin/lazysky.rs    the LazyOS launcher and its Platform
```

The dependency is one way, `bin -> lazysky-ui -> lazysky-core`;
`cargo tree -p lazysky-core` lists no `xui-*` crate. Everything OS-specific
the core needs is the `Platform` trait (CA roots, app folder, pictures folder,
open a link, a log line), implemented by the bin and by the desktop example.

The window never waits (the plan's R1 to R8): requests go to the core, text
is shaped on a layout thread, pictures are decoded on image threads, and a
timer drains the answers within a 4 ms budget, 16 ms while work is pending
and 250 ms when idle. The feed keeps its scroll position as an anchor row
and an offset into it, so rows arriving above the view never move it.

## Verifying it

```bash
cd xui-app
cargo test -p lazysky-core -p lazysky-ui            # protocol, model, pictures, saving, windows
cargo test -p lazysky-ui --release --test bench -- --ignored --nocapture   # paint p99 vs 4 ms
cargo clippy -p lazysky-core -p lazysky-ui --all-targets -- -D warnings
cargo tree -p lazysky-core | grep -c "xui-core\|xui-canvas"                # must be 0
python ../tools/nettls/licenses.py --lazysky                                # GPLv2-compatible tree
```

The offscreen window tests write `xui-app/target/snapshots/lazysky-*.png`:
look at them. The `xui-app` lib and bin tests are Linux-only; on Windows the
LazyOS binary is built for musl with `tools/xui/build.py`.

Serial markers the app and the core print:
`SKY:UP:PASS`, `SKY:LOGIN:PASS:<handle>` / `SKY:LOGIN:FAIL:<reason>`,
`SKY:FEED:PAGE:<posts>:<skipped>`, `SKY:FEED:ROWS:<n>`, `SKY:DETAIL:ROWS:<n>`,
`SKY:IMAGES:<n>`, `SKY:LIKE:ok|fail`, `SKY:REPOST:ok|fail`,
`SKY:POST:PASS:<uri>`, `SKY:DELETE:ok:<collection>`,
`SKY:SAVE:DONE:<path>:<bytes>` / `SKY:SAVE:FAIL:<reason>`,
`SKY:BULK:DONE:<n>:<errors>`, `SKY:SETTINGS:SAVED:<path>`,
`SKY:PERF:paint_p50=<ms>:paint_p99=<ms>:paint_max=<ms>:tick_max=<ms>:frames=<n>`
every two seconds while scrolling, and `SKY:PERF:TOTAL:...` and
`SKY:QUIT:PASS` on close. A password or token is never printed.

## Licences

The crates declare GPL-3.0-or-later. Every linked third-party crate has a
GPLv2-compatible licence choice (`tools/nettls/licenses.py --lazysky`); never
add `ring`, `aws-lc-rs`, `tokio`, `reqwest` or an Apache-2.0-only crate. The
host's trust store (`rustls-native-certs`) is only behind the core's
`native-roots` feature, which LazyOS images never link.

## Live run and crash reports

`python tools/lazysky/live.py` (needs `LAZYSKY_DUMMY_ACCOUNT` and
`LAZYSKY_DUMMY_PWD`, an app password of a throwaway account) builds an image
with no test CA, signs in to the real service, scrolls and clicks "Load more"
in a loop, and fails on a second `SKY:UP:PASS` (a restart), a `SKY:PANIC:`
line, a failure marker or the password in the serial log. The release
profile is `panic = "abort"`, so any panic on any thread ends the app: the
panic hook in `src/bin/lazysky.rs` prints `SKY:PANIC:<message> at
<file:line>` first, which is what a crash report should quote.

Findings so far: `MouseWheel.delta` is in 120ths of a notch (divide by
`WHEEL_DELTA`); a video post has no thumbnail path and shows a blank block;
characters outside Droid Sans (mathematical bold letters) draw as boxes. A
user's crash while scrolling was `SKY:PANIC:RefCell already borrowed at
feedview/events.rs`: an event reached the feed's handler while the tick (or a
paint) held its state, and `panic = "abort"` turned the panic into an app
restart. The handler and the tick now use `try_borrow_mut` and defer or drop
instead; the mock and the live runs never hit it, so a real user's timing was
needed.
