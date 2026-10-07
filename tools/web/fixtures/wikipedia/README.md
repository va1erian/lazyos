# Wikipedia copies for the LazyWeb harness

Copies of two English Wikipedia pages, the
[Main Page](https://en.wikipedia.org/wiki/Main_Page) and
[1762](https://en.wikipedia.org/wiki/1762), as LazyWeb asks for them
(`?useskin=vector`), with the style sheets and pictures they reference.
`tools/web/run.py` serves them from the host in place of the real sites
(`tools/web/wiki.py`), since the sandbox and CI cannot reach Wikipedia.
`python tools/web/wikicapture.py` fetches them again; `manifest.json` maps each
URL to its file in `files/`.

Licences: the text of the pages is by Wikipedia's contributors, under
[CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/); the history
of each page lists its authors. Each picture is under the licence its file
page on Wikipedia or Wikimedia Commons gives (the Main Page shows only freely
licensed pictures). The Wikipedia logo and wordmark are trademarks of the
Wikimedia Foundation, used here only to test the browser's rendering of the
site. The style sheets are MediaWiki's (GPL-2.0-or-later).
