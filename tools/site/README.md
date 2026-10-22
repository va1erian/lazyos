# Documentation site (GitHub Pages)

The site at <https://va1erian.github.io/lazyos/> is generated from the Markdown
in this repository; it replaces the GitHub wiki, which is no longer used. There is
nothing to edit on the site itself: change a file under `docs/` and the next CI run
publishes it.

## What goes in

- every `*.md` under `docs/` becomes a page, at the same path with `.html`
  (`docs/architecture/boot.md` is `architecture/boot.html`);
- the home page (`index.html`) is the first of `home` in `site.toml` that exists:
  `docs/index.md`, the start page the Docs app opens, else `README.md`. `README.md` is
  otherwise a page of its own (`readme.html`, "README: building and booting"), and
  `CONTRIBUTING.md` another (`[extras]`);
- every other file under `docs/` (logs, text captures, `docs/design/*.html`) is copied
  beside the pages, so a relative link to it keeps working;
- a docs file that would overwrite a generated page (`contents.html`, `404.html`, another
  page, the theme) fails the build;
- the first `# heading` is the page's title; the sidebar groups pages by directory (the
  longest `[nav.directories]` match names the section, so `docs/rhai/api` is separate
  from `docs/rhai`) and, for the top level, "Plans" for `*-plan.md` against everything
  else, with the pages named in `overview` first. All of it lives in `site.toml`.

The OS image embeds the root `README.md` and `THIRD_PARTY.md` as `docs/README.md` and
`docs/THIRD_PARTY.md`, so docs link to them by those names; `[aliases]` makes such a
link mean the root file on the site (a real file under `docs/` wins). `[titles]`
overrides a page's title where its first heading is not the right label.

Write docs for GitHub as before: relative links to other `.md` files, to files
outside `docs/` (they link to the file on GitHub at the site's `ref`), and
`#fragments` that follow GitHub's heading ids all keep working. The generator
rewrites them; every page URL it emits is relative, so the output works under any
path (only `404.html`, served from any path, needs the base URL: `/<repo name>/`, `/` for
an `<owner>.github.io` repository, or `SITE_BASE_URL`; `--base-url` on the command line).
Serve it over http (`python -m http.server`): search fetches its index, which
`file://` forbids. It adds a search box (a JSON index fetched on first use), a table of contents per
page, a light and a dark theme, code highlighting, and an "All documents" index.

## Build it

```bash
pip install -r tools/site/requirements.txt      # markdown-it-py, Pygments (pinned); Python 3.11+
python tools/site/build.py                      # -> target/site
python tools/site/build.py --strict             # what CI runs: broken links fail
python -m http.server -d target/site 8000       # then open http://localhost:8000/
python tools/site/test_site.py                  # the generator's own tests
```

`--strict` fails on a link to a file that does not exist and on a `#fragment` (in the
same page or another) that matches no heading of the page it points to. Images outside
`docs/` link to GitHub's `raw` URL; bare file names such as `AGENTS.md` are not autolinked. Renaming a doc means fixing its links
in the same pull request, and CI says which ones. Options: `--out DIR`, `--config
FILE`, `--base-url /path/` (only for `404.html`, default `/<repo name>/`).
`GITHUB_REPOSITORY` and `SITE_REF` override the repository and the ref that source
links point at (`site.toml`: `va1erian/lazyos`, `main`).

## CI

`.github/workflows/pages.yml` runs the tests, builds with `--strict` and deploys
with `actions/deploy-pages`:

- on a push to `main` that touches `docs/**`, `README.md`, `CONTRIBUTING.md`,
  `tools/site/**` or the workflow;
- after every `abi-compat` run on `main`, because the Linux ABI matrix and syscall
  coverage (`docs/compat/matrix.md`, `coverage.md`) are CI output, git-ignored, and
  taken from that workflow's newest `abi-reports` artifact (a GitHub artifact expires,
  by default after 90 days); without one the pages are absent and the build warns;
- deploys happen only from `main`;
- on a pull request it builds and checks the site without deploying;
- by hand: Actions -> pages -> Run workflow.

A link to a file CI generates (`[generated]` in `site.toml`) is not an error when the
file is missing from a local checkout.

One-time repository setting: Settings -> Pages -> Source: **GitHub Actions**. The
wiki itself can be switched off under Settings -> Features -> Wikis.

## Layout

| File | Role |
|---|---|
| `build.py` | CLI: discovers pages, renders, writes the site, search index, `404.html`, checks |
| `collect.py` | finds the pages and groups them into sidebar sections |
| `render.py` | Markdown to HTML (GitHub-flavoured), heading ids as GitHub makes them |
| `links.py` | rewrites links: page, copied file, or GitHub URL for code |
| `theme.py`, `assets/` | page template, `style.css`, `site.js` (theme, menu, search) |
| `site.toml` | title, repository, sidebar order, extra pages |
