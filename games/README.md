# games/ — the standalone game ports

The games that ship as their own `.lzp` packages live here, each a
self-contained Cargo workspace built by a tool under `tools/`:

| Port | Crate | Built by | Package |
|---|---|---|---|
| [`doom/`](doom/) | doomgeneric (C) + our platform layer | `python tools/doom/build.py` | `org.lazy.doom` (with the Freedoom IWAD) |
| [`quake/`](quake/) | `quake-srp` (Rust) + our bridge | `python tools/quake/build.py` | `org.lazy.quake` (with id's shareware pak) |
| [`srb2kart/`](srb2kart/) | Kart-Public v1.6 (C) + our backend | `python tools/srb2kart/build.py` | `org.lazy.srb2kart` (engine only; data fetched at build time) |

Each port is a *user* package: the image ships the `.lzp` in
`/system/share/samples` and the player installs it with `pkgctl install` (or
by opening it in Files), which is why they live outside the app workspace.

The two games that ship as *core packages* — LazyGolf
(`xui-app/crates/golf`) and the NES Player (`xui-app/crates/nes`) — are core
apps: `tools/xui/build.py` builds them as members of the `xui-app` workspace
and packages them from `xui-app/packages/`, so they stay where that build
expects them. They are optional: only a `LAZYOS_GAMES=1` desktop image
(`run_demo.py --games`, the launcher's Games group) installs them. The split is
by how a game is shipped, not by what it is.

Nothing here is in the OS workspace: `cargo build` at the repository root
never builds a port, and each port's own `cargo test` (host) or its harness
(`tools/doom`, `tools/quake`, `tools/srb2kart`) is how it is checked.
