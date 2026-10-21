# Web Engine (webd)

webd draws web pages for other apps. An app that only needs to show formatted text and pictures (a
message in a mail reader, a rendered document, a help page) does not carry a web engine of its own: it
asks webd for a **light view**, gives it the page as HTML, and gets the page's pixels back. The engine
is WebKit, the one in Safari, ported to LazyOS; webd keeps one copy of it for the whole login session.

webd has no window. It starts by itself when you log in, runs in the background and shows an icon in the
taskbar's tray; you do not open it. Closing it from the tray (or logging out) ends the pages other apps
were showing; they say so and can start it again.

## What a light view can and cannot do

* It shows only what the app gives it. It never loads anything from the network: a picture, a style
  sheet or a script that is not inside the page is not fetched, and the page cannot tell it to.
* A link you click, a form you submit and a refresh the page asks for load nothing; the app is told which
  address was meant and decides what to do.
* Scripts are off unless the app asks for them. Even then a page has no network, no storage, no
  cookies and no windows.
* It cannot read your files. Its page process is sandboxed: it reads only fonts.

## Where it keeps things

One folder, `~/.apps/os.lazy.webd`: the engine's rule store (what keeps a page off the network), built
again at every start. No history, cookies or caches are kept.

## For developers

`docs/webd.md` in the source tree describes the interface (`idl/webd_light.midl`, served as
`app.os.lazy.webd.light`), the markers it prints (`WEBD:*`) and its self-test, which an image with webd
runs when the app is launched again with the path `/selftest`
(`rhai -e 'sys::init::launch("os.lazy.webd", "/selftest", 0)'`).
