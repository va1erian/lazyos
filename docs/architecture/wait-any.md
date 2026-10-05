# `wait_any`: one park for endpoints, calls and subscriptions

Design note for issue #309. Code: `kernel/src/ipc/channels/recv/waitset.rs`
(the op), `recv/waitcall.rs` (call items), `user/src/messenger/wait.rs` (the
client), `user/src/messenger_async/selector.rs` (`Selector`). Tests:
`kernel/src/tests/waitset_suite.rs`, `waitset_ext_suite.rs`,
`waitset_call_suite.rs`; demo: `user/src/bin/async_echo.rs` (`wait.rs`).

## The op

Native Messenger op `wait` (19). `MsgArgs::parcel_ptr` points at
`parcel_len` 64-bit words (at most `MAX_WAIT_ENDPOINTS` = 8), `flags` holds
doorbells and `WAIT_DEADLINE_NS`, `deadline` is absolute (ticks, or
monotonic nanoseconds with `WAIT_DEADLINE_NS`; 0 is "none"). It returns a
ready mask: bit `i` for word `i`, bits 59..63 for the doorbells. Nothing is
consumed: the caller then takes what is ready with its normal operation
(`try_recv`, `await_reply`, draining the bus), so every rule of those paths
(transfers, quotas, poll grace) stays in one place.

## Handle kinds

| Item | Word | Ready when | Wake source |
|------|------|------------|-------------|
| Endpoint | the handle number (needs `CALL` rights) | inbox not empty, or the peer side closed, or the channel vanished | registered on the endpoint's waiter list, under the `CHANNELS` lock that saw it empty, exactly like `recv` |
| Pending call | `txn_id \| WAIT_ITEM_CALL` (bit 63) | the transaction is terminal (replied, timed out, canceled, peer died) | none needed: every terminal transition already wakes the transaction's caller on the `MESSENGER` queue, the queue the wait parks on |
| Doorbell | a bit in `flags` | raw input, display keys, `AF_INET` work, a finished child, a readable descriptor | each bell's own arm/disarm (`arm_doorbells`) |

Transaction ids are a sequence number shifted above the registry slot
(`channels/registry.rs`); with 64 slots they reach bit 63 only after 2^57
calls, and handle numbers are small, so the tag cannot collide.

A call item must exist and belong to the caller (`NoTransaction` -> `ENOENT`,
`NotCaller` -> `EPERM`); the check is at entry, so a task never observes or
times another task's transaction.

### Subscriptions

Topic delivery stays pull-based in `messengerd`. A subscription takes part in
a wait in either of two ways, both of which are plain items above:

* its **doorbell** (`central::Subscription::bell`, P7.2): an endpoint the
  broker sends one `Ready` on when events are waiting and nobody pulls. This
  is the recommended form: the subscriber parks on the bell beside its own
  endpoints, then drains `NextEvent` with an expired deadline (which re-arms
  the bell).
* its outstanding **`NextEvent` call**: begin the pull with `begin_call` and
  put the transaction in the set; it is ready when the broker answers it. The
  synchronous-call cycle rule applies: while that call is open the task may
  not make another call on the same broker channel (`Deadlock`), so this form
  suits a task that only listens.

## Wake semantics

One pass (interrupts off, one CPU, so nothing slips between the check and the
park): under the `CHANNELS` lock, each endpoint is checked and registered and
each call's state is read; then the doorbells are armed. Any ready item ends
the wait with the full mask of everything ready at that moment (duplicates
included: the same handle twice sets both bits). Otherwise the task parks on
`MESSENGER` until the earliest of the caller's deadline and the pending
calls' own deadlines. Every wake is advisory: registrations are dropped and
the pass runs again.

## Deadlines

* The caller's deadline ends the wait with `TimedOut` (`ETIMEDOUT`).
* A pending call's own deadline is otherwise enforced only by `await_reply`
  (it expires the transaction when its caller's park times out). A task parked
  in `wait` is not in `await_reply`, so the wait parks no longer than the
  earliest call deadline, expires the due calls itself (`expire_transaction`,
  a no-op when a reply won the race) and reports them ready; the later
  `await_reply` returns `TimedOut`. Only when the caller's own deadline has
  passed and no call became ready does the wait return `TimedOut`.
* Deadline versus wake: a reply racing the deadline wins (expiry ignores a
  terminal transaction), and a wake and a timeout in the same pass both end
  in a rescan, so a ready item is never lost to a timeout.

## Cancellation and races

* **Cancel**: only the caller may cancel a transaction, and it is parked; a
  canceled call put in a later set is ready at once and awaits `Canceled`.
* **A fatal signal or kill** ends the wait with `Canceled` (`ECANCELED`), as
  for `recv`; the calls stay pending for the caller (or its teardown) to
  finish.
* **Peer death mid-wait**: closing either side marks the pending calls
  `PeerDied` and wakes their callers, and wakes every waiter of both sides; the
  endpoint item reports ready and its receive reports `PeerDied`.
* **Handle closed while waiting**: the wait holds no reference. A vanished
  channel counts as ready so the following receive reports it; a stale
  registration is dropped by id and never wakes an unrelated wait (wakes only
  reach a task still parked on `MESSENGER`).
* **A transaction awaited by someone else**: impossible, since only its
  caller may name it.
* **Duplicate items**: allowed; each sets its own bit.

## Limits, metering, validation

At most 8 items and the known doorbells (`BadParcel` -> `EINVAL` otherwise);
the word array is copied in through the syscall layer's checked `copy_in`
before any lookup. The wait allocates nothing per item (a fixed array on the
kernel stack) and charges no quota: the calls it watches were metered when
begun (`MAX_OUTSTANDING`, `MAX_PENDING_PER_SENDER`).

## `Selector`

`Selector::step` takes queued one-way messages first, then parks once on
every in-flight call and queued receive (`wait_items`, calls first) and
reports the first ready item, so a newer call finishing first is reported
first and a message wakes it while calls are in flight. With more than 8
items it watches a rotating window of 8 with a one-tick deadline, so every
item can still wake a step within a few ticks.
