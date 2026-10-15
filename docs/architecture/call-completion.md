# Call completions: one wait for every call in flight

Design note for issue #725. Code: `kernel/src/ipc/channels/completion.rs`
(the queue and the doorbell), `kernel/src/ipc/channels/recv/waitset.rs` (the
`WAIT_CALLS` arm), `kernel/src/ipc/syscalls.rs` (`CALL_CHECK` and
`CALL_DRAIN`), `user/src/messenger/endpoint.rs` (`await_reply_now`) and
`user/src/messenger_async/future.rs` + `executor.rs` (the real `Pending`
futures). Tests: `kernel/src/tests/completion_suite.rs` (correctness and
soak, including cancel and peer death).

The state before (#716's compromise): `user/src/messenger_async.rs` was
async-shaped but every leaf future parked the task in the kernel
(`OP_CALL_AWAIT`), so several calls could be *in flight* but a loop could not
stay responsive while one was pending; a UI solved it with workers and a
cross-thread waker (`xui-app/src/backend/wakeup.rs`), which works but costs a
thread, a `Send` copy of the state and a per-thread service handle table.

## The pieces

**A per-task completion queue.** Every terminal transition of every call
`record`s its transaction in the *caller's* queue: a reply, the call's own
deadline (a wait's timer expiry, `expire_transaction`), a cancel the caller
issued, the peer closing its side, a served poll the callee never answered.
The queue is one bounded ring per task slot (64 entries, oldest dropped —
a dropped entry only costs a wake the doorbell no longer reports; the
caller's own await still finds the terminal state), and `forget_task`
clears the dying slot's entry so a reused slot inherits nothing.

**One doorbell in `wait`.** `WAIT_CALLS` is a doorbell flag, not a set
item: `wait`'s pass reports `CALLS_READY` while the queue is non-empty
(level, like `WAIT_CHILD`), and `record`'s ring wakes a parked task
directly. The whole point of the shape: a loop's park names its window
endpoints (1-2 of the 8), the `WAIT_FD` descriptor and this one flag, and
*every* call in flight rides along — no per-call item, no overflow-to-poll
strategy, and nothing else in the wait set is spent.

**Two ops.**

* `CALL_CHECK`: a terminal transaction's outcome **now**, without the park
  `CALL_AWAIT` is (`-EAGAIN` while open). Nothing is consumed for a pending
  transaction; a terminal one is consumed exactly as `await_reply` would.
* `CALL_DRAIN`: up to `buf_cap/8` finished transaction ids off the queue,
  oldest first.

The kernel refuses unknown flags on every op, so adding the doorbell to the
wait set keeps the ABI pinned (tests: `wait_flags_known`).

**Deadlines the caller never parked on.** A call whose caller waits on the
doorbell alone is not named as a `WAIT_ITEM_CALL`, so its deadline would
never fire (expiry lived in `await_reply` and in the named items' wait
expiry). A `WAIT_CALLS` wait therefore computes the earliest deadline of all
of the task's still-pending calls (`completion::caller_deadline`, one
registry pass at arm time) into its park bound, and on a timed-out park
expires the due ones (`completion::expire_due`) — the same accounting
`expire_transaction` does — and rescans. A raced reply still wins. This is
the one place the design differs from "the doorbell is free": the arm-time
scan is O(open transactions of all channels) for a task with calls pending;
a task with none (the usual case) pays a registry-empty pass behind the
existing `wait` lock.

## The userspace shape

`messenger_async::Call`'s poll now returns a real
[`Poll::Pending`](core::task::Poll::Pending): it checks with `CALL_CHECK`
and Pending until the transaction is terminal. It tracks its transaction in
a small registry (`future::pending`); the `Executor`'s and `block_on`'s
idle park is a `wait_items` over at most `MAX_ENDPOINTS` of those ids
(the completion-queue semantics make the *doorbell* strictly better, the
ids only tell the wait which queue to wake); a stale id (a dropped future)
is checked and pruned. `Selector` is unchanged: it names call items
directly, and a ready call's poll now resolves instead of parking.

The same doorbell is what lets a UI loop drop the worker for a quick call:
`park_client` adds `WAIT_CALLS` beside its epoll set, begins calls with
`CALL_BEGIN`, and drains completed transactions (`CALL_DRAIN` + `CALL_CHECK`)
on the tick the wake reports. That integration is the staged follow-up
(the workers stay for the genuinely long operations: prompts, installs,
pastes); the pieces it needs are all here.

## Not done here

* The transaction itself keeps its reply bytes until the caller's own
  `CALL_CHECK`/`CALL_AWAIT` consumes them — completions carry ids, not
  bytes, so a slow drain does not grow the kernel heap per call.
* No wall-clock composition beyond the deadline handling above; a wait
  that wants timers composes through the `WAIT_DEADLINE_NS` flag as before.
