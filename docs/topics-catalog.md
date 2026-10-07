# Ad hoc topic declarations (catalog for #307)

**Status: every topic below is now declared in `idl/*.midl` (PR #388 and its follow-up). The table is kept as the historical pre-migration snapshot.**

Snapshot of every Messenger topic that is named and encoded by hand, taken on
`main` at `1a3ac94`, before topics can be declared in MIDL. Each row is a
candidate for a `topic` declaration in `idl/*.midl`.

`retained` is the flag the publisher passes today. "Payload" describes the
current hand-built encoding. Only `session/+/clipboard/changed` and
`system/confd/changed/#` use a real TLV codec; the rest are `key=value` text.

| Topic pattern | Publisher | Subscribers | Retained | Payload today | Where declared by hand |
|---|---|---|---|---|---|
| `session/<id>/clipboard/changed` | clipboardd | clipcopy, clippaste, `clipboard::Client::subscribe_changes` | yes | TLV `OfferMeta` (struct exists in `idl/clipboard.midl`, topic name not) | `user/src/messenger/clipboard/mod.rs:130` (`changes_topic`), `protocol.rs:154` |
| `user/<uid>/confd/changed/<path>` | confd | `xuid` (the shell user's `ui/#`, issue #407), `confctl watch user/<uid>/confd/changed/#` | no | `Change` (`idl/confd.midl`); only that uid and root may subscribe (`kernel/src/ipc/topics/private.rs`) | `user/src/bin/confd.rs` (`TopicSink::changed`), `user/src/bin/xuid/themefeed.rs` |
| `system/confd/changed/<path>` | confd | confctl (default `system/confd/changed/sys/#`), timed (`sys/time/zone`) | no | hand-coded `(path, deleted)` payload | `user/src/messenger/confd.rs:161-163` (`change_topic`, `change_payload`), `user/src/bin/confd.rs:165-172`, `confctl.rs:34`, `timed/state.rs:20` |
| `time/tick` | timed | not verified | yes | text | `libs/timed/src/lib.rs:26`, duplicated in `user/src/messenger/timed.rs:100` |
| `system/stats/memory` | sysmond | not verified; `starts_with("system/stats/")` filter in sysmond | yes | `key=value` line | `user/src/bin/sysmond.rs:196,227` |
| `system/stats/tasks` | sysmond | not verified | yes | `live=N` line plus one line per task | `user/src/bin/sysmond.rs:204,242` |
| `system/health/<name>` | healthd (via `TopicBroker`) | logd (`system/health/#`), init | yes | `status=<s> detail=<d>` | `user/src/bin/healthd/aggregate.rs:250`; `health_topic` strings in `user/src/bin/init/state.rs:132-248,373` (12 fixed + per-app) |
| `system/health/summary` | healthd | not verified | yes | `status=<s> detail=<d>` | `user/src/bin/healthd/aggregate.rs:48,84` (duplicated) |
| `system/events/service/<name>` | init supervisor | healthd (`.../service/#`), logd | yes | `state= pid= restarts= status= health= [detail=]` | `user/src/bin/init/supervise.rs:277-285`; prefix const `healthd.rs:57` |
| `system/events/login/start` | logind | logd | configurable | `user= uid= session= pid= state=` | `user/src/bin/logind.rs:174-200` |
| `system/events/login/session/<id>` | logind | logd | configurable | `user= uid= pid= state=` | `logind.rs:185` |
| `system/events/login/denied` | logind | logd | configurable | `user= reason=` | `logind.rs` (around 29 of the denied block) |
| `system/events/login/end` | logind | logd | configurable | `user= uid= session= status=` | `logind.rs:219` |
| `system/events/open/<app>` | mimed | not verified (intended: the target app) | no | `path= mime= verb=` | `user/src/bin/mimed/handlers.rs:102-104`, `164-174` |
| `system/events/clipboard/paste` | clipboardd | logd | see `publish_event` | free-text `paste #N uid= session= mime= app= bytes= lazy=` | `user/src/bin/clipboardd/handlers.rs:221,241` |
| `system/events/security/clipboard` | clipboardd | logd | see `publish_event` | free-text `deny #N uid= session= mime= app= token=` | `clipboardd/handlers.rs:236` |
| `system/events/#` (filter) | n/a | logd | n/a | n/a | `user/src/bin/logd.rs:212` |
| `system/health/#` (filter) | n/a | logd | n/a | n/a | `logd.rs:218` |

Declared and published since this snapshot:

| Topic pattern | Publisher | Subscribers | Retained | Payload | Declared in |
|---|---|---|---|---|---|
| `system/audio/mixer/event` | audiod (`_audio`) | apps on `os.lazy.audio` (`beep starve=1`) | no | `AudioEvent {stream, kind, frames}`: `Underrun` once per dry spell, `Drained`, `Period` (rate-limited, only while subscribed) | `idl/audio.midl` (#453) |
| `system/audio/virtio-snd0/event` | sndd (`_snd`) | the mixer, diagnostics | no | `AudioEvent`: the card stream's `Underrun`, `Drained`, `DeviceError` | `idl/audio.midl` (#453) |
| `system/events/app/<id>` | init (central broker) | LazyShell (`system/events/app/+`) | yes | `AppFailure {name, status, summary, reason, session, startup, at}` | `idl/init.midl` (#549) |
| `system/events/elevd/request` | elevd (`_elev`) | logd (journalled to `/logs/elevd.log`) | no | `Record`: one per request, whatever came of it (granted, refused, cancelled, held, busy, ...) | `idl/elevd.midl` (docs/accounts-plan.md U2, #625) |
| `session/<id>/apps/resident` | init (central broker) | LazyShell (default tray items) | yes | `ResidentApps {apps: [ResidentApp {app, pid}]}`: the session's running resident apps | `idl/init.midl` (docs/tray-plan.md, #633) |
| `session/<id>/shell/tray` | LazyShell | `xui_app::tray`, `libs/trayclient` (call `Set` again on a new generation) | yes | `Generation {generation}` | `idl/tray.midl` (docs/tray-plan.md, #633) |

`messengerd` admits `system/` publishes from uid 0, plus `_snd`/`_audio`
under `system/audio/` only (`sndpolicy::may_publish_audio_event`), and each
dedicated system uid under its own subtree (`messengerd/filter.rs`: `_devd`
`system/devices/`, `_elev` `system/events/elevd/`, `_net` and `_netd` their
link and stack topics).

Test-only or probe topics, not to be declared: `system/events/selftest/forbidden`
(`messengerctl/selftest.rs:73`), `system/events/network[/up]`, `system/events/t<N>`
and `session/1/clipboard/changed` in `libs/generated/tests/*`, and the kernel
topic-gate fixtures in `kernel/src/tests/topics_*_suite.rs`.

## Observations

- `confd` is the renamed `regd`, so `system/confd/changed/<path>` is the
  `(path, deleted)` topic the issue calls `system/regd/changed/<path>`.
- Duplicated names: the `system/health/summary`
  payload is built in two places in `aggregate.rs`.
- Publisher-side ACL: `messengerd/filter.rs:94` gates the `system` prefix by hand;
  the `publish:` / `subscribe:` permission strings that would replace it are
  documented only in `docs/security-model.md` section 6 (example at line 194).
- Payload formats are inconsistent: two topics use TLV, the rest use ad hoc `key=value`
  text. Text payloads need a declared struct (or a `Text` payload type) so they
  can be typed too.
- Retained flag is chosen per call site for login events (`publish(..., retained)`),
  so the declaration needs to settle one value per topic.
- The MIDL `Qos` default per topic is not recorded anywhere today; subscribers pick
  it at subscribe time (`user/src/messenger/topics_client/client.rs:254`).
