# LazySky

A Bluesky client for LazyOS (and Windows, Linux and macOS).

* **Sign in** with your handle and an **app password** (create one in
  Bluesky: Settings, Privacy and security). The password is never saved; the
  handle is only when you tick *Remember my handle* in Settings.
* **Read** the timeline, a profile (Posts, Media, Likes), your bookmarks,
  notifications and search results (accounts or posts). Click a post for its
  thread; `Alt+Left` or *Back* goes back.
* **Write**: like, repost, reply and post with up to four pictures; delete
  your own posts from the "..." menu.
* **Pictures**: click one for the viewer (`Ctrl+=`, `Ctrl+-`, `Ctrl+0` zoom;
  drag to pan; `Ctrl+S` saves). Right-click a picture for *Save image as...*.
  On a profile, *Download images* saves every picture of the account's own
  posts into `~/Pictures/LazySky/<handle>/`.

LazySky keeps scrolling smooth by never waiting on the network, on decoding
or on text layout: those run on their own threads. See `docs/lazysky.md` in
the source tree.
