# LazyAgent: the AI chat and diagnostics agent

LazyAgent (`os.lazy.agent`, binary `xui-agent.elf`, menu name **AI Agent**) is a
desktop chat client for a large language model on **Fireworks AI**, on xui. It
runs as a LazyOS window and, from the same code, as a Windows, Linux or macOS
window. The design, the trust model and the phases are in
[`ai-agent-plan.md`](ai-agent-plan.md); this page is how to use and build what
exists. Today that is the chat (P2): diagnostics, the `diagd` service and the
GitHub issue flow come in later phases.

## Using it

* **First run.** A consent screen says what is sent where: everything you type
  and every reply goes to Fireworks AI, outside this computer; conversations
  stay on disk in the app's folder, readable by you only; the key is never
  written to disk.
* **Key.** Paste your Fireworks API key. It is kept in memory until you close
  the app (Settings, *Forget the API key*, drops it sooner) and is asked again
  next run. A key Fireworks refuses is forgotten and asked for again.
* **Chat.** Type in the box at the bottom: `Enter` sends, `Shift+Enter` adds a
  new line. The reply streams in as Markdown: headings, paragraphs, lists,
  quotes and code blocks (each with a **Copy** button); a model's reasoning is a
  collapsed *Thinking* line you can open. A link opens only after you confirm
  its address. `Esc` or **Stop** ends a reply (what arrived is kept and marked
  interrupted); **Redo** asks the last question again.
* **Conversations** are listed on the left, kept in
  `~/.apps/os.lazy.agent/conversations/` (one append-only file each), named by
  the model after the first answer, searchable (the search box), exportable as
  Markdown (**Export**) and deleted into a trash folder, never hard-deleted.
  `Ctrl+N` starts a new one.
* **Settings** (`Ctrl+,`): the model (five presets or any id), reasoning
  effort, theme, and a line added to the system prompt. They live in
  `settings.toml` in the app's folder; no secret is ever in it.
* When the conversation grows past 75% of the model's context the oldest turns
  are replaced by a summary the model writes (a note says so).

## Running it

```bash
python tools/run_demo.py --agent            # desktop + networking + LazyAgent
python tools/lazyos_gui.py                  # Simple tab, Web group: "AI Agent"
cargo run --release --manifest-path xui-app/Cargo.toml -p lazyagent-ui \
    --example desktop --features desktop     # the same window on Windows/Linux/macOS
FIREWORKS_API_KEY=... cargo run -p lazyagent-core --bin agent_cli --features native-roots
```

`LAZYOS_AGENT=1` needs `LAZYOS_DESKTOP=1` and `LAZYOS_NETD=1`; the build refuses
it otherwise (`build_support/agent_embed.rs`). It does not need the HTTPS tools
(`LAZYOS_TLS`): the CA bundle is in every image. The core package
(`xui-app/packages/agent`) holds the window, sockets (`outbound`), `mimed`, the
clipboard and the app's own folder; derive any further permission from a
`LAZYOS_LABEL_TRACE=1` run (`python tools/agent/run.py --label-trace` must show
no `LABEL:DENY`).

## How it is built

```text
xui-app/crates/https        TLS, roots, resolver and capped/streamed requests, shared with LazySky
xui-app/crates/agent-core   no xui, no LazyOS: Fireworks client, SSE, conversations, the Core
xui-app/crates/agent-ui     the portable xui window (TranscriptView, pages, Markdown)
xui-app/src/bin/agent.rs    the LazyOS launcher and its Platform
```

The dependency is one way, `bin -> agent-ui -> agent-core -> https`;
`cargo tree -p lazyagent-core` lists no `xui-*` crate. Everything OS-specific
the core needs is the `Platform` trait (CA roots, app folder, endpoint, a link
to open, a log line).

The window never waits (rule 1 of the plan): the `Core` worker thread owns the
HTTPS client, the key and the store; Markdown is parsed and text shaped on a
layout thread; a timer drains both within a 4 ms budget, 16 ms while work is
pending and 250 ms when idle. The transcript is one custom-painted,
virtualized view whose scroll position is an anchor row and an offset into it,
so a streaming reply that grows does not move what you are reading; while the
view is at the bottom it follows the end. Stop works while bytes keep arriving:
the stop flag is looked at before every read of the socket (`lazyhttps::Watch`).

The only hosts the app may reach are `api.fireworks.ai` (and, later,
`api.github.com`), over HTTPS, with no redirects: a policy in the HTTP layer,
tested in the core (the package's `outbound` permission lets the label reach
any host, so this is the app's own discipline).

## Verifying it

```bash
cd xui-app
cargo test -p lazyhttps -p lazyagent-core -p lazyagent-ui   # SSE + fuzz, provider, store, core, windows
cargo clippy -p lazyhttps -p lazyagent-core -p lazyagent-ui --all-targets -- -D warnings
FUZZ_CASES=100000 cargo test -p lazyagent-core --release fuzz::
python ../tools/nettls/licenses.py --lazysky --agent               # GPLv2-compatible trees
python ../tools/agent/run.py                                       # QEMU: mock Fireworks, window, judge
python ../tools/agent/test_judge.py                                # the judges fail when they should
python ../tools/agent/run.py --live                                # the real service (manual, FIREWORKS_API_KEY)
```

The offscreen window tests write `xui-app/target/snapshots/agent-*.png`: look
at them. `tools/agent/run.py` binds host port 443 for its mock (check no other
harness holds it), serves `api.fireworks.ai` under a throwaway CA, types a wrong
and then the right key (`{"type_secret": "AGENT_TEST_KEY"}`, random per run),
chats, scrolls and stops a stalled reply, and judges the markers, the mock's
record (host, SNI, the key only in `Authorization`, the default model, no
tools), the serial log (no key) and the screenshots. `p0_guest.py` is the P0
spike harness (a console image streaming from the guest).

Serial markers (channel `AGENT`, through `xui_app::trace`): `AGENT:UP:PASS`,
`AGENT:READY:PASS`, `AGENT:CONSENT:PASS`, `AGENT:KEY:PASS:SET`,
`AGENT:CHAT:SEND:<turns>`, `AGENT:CHAT:FIRST:<ms>`,
`AGENT:CHAT:DONE:<in>:<out>:<ms>`, `AGENT:CHAT:FAIL:<status>`,
`AGENT:ROWS:<n>`, `AGENT:CONV:<n>`, `AGENT:FOLLOW:<0|1>`, `AGENT:COPIED`,
`AGENT:LINK:ASKED|OPENED`, `AGENT:QUIT:PASS`. No marker carries message text, a
link, a key or a token.

## Licences

The crates declare GPL-3.0-or-later. Every linked third-party crate has a
GPLv2-compatible licence choice (`tools/nettls/licenses.py --agent`); never add
`ring`, `aws-lc-rs`, `tokio`, `reqwest` or an Apache-2.0-only crate.
`pulldown-cmark` (MIT) parses the Markdown.
