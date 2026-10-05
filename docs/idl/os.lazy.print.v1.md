# `os.lazy.print.v1`

Interface id: `0xbf8d5aec16ec445f`

The print spooler, `printd` (docs/printing-plan.md P6).

An app opens a job, writes its whole document with `Write` and queues it
with `Close`; only a closed job is sent to its printer, so a printer never
gets half a request because the app quit. A job answers only to the uid
that opened it (and root): the uid is the kernel-stamped sender, never a
wire field, and anyone else is told the job does not exist. Failures are
the shared structured error field, `EINVAL` with one line for the user
(`There is no such print job`, `The print queue is full; ...`).

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Open | 1401622761 | sync | `(printer: String, user: String, ticket: Ticket) -> (job: U32)` |
| Write | 1717336572 | sync | `(job: U32, bytes: Bytes) -> ()` |
| Close | 1300671683 | sync | `(job: U32) -> ()` |
| Cancel | 900713019 | sync | `(job: U32) -> ()` |
| Status | 6222351 | sync | `(job: U32) -> (info: JobInfo)` |
| Jobs | 1256630173 | sync | `() -> (jobs: Array<JobInfo>)` |

## struct `Ticket`

- `name: String`
- `format: String`
- `copies: U32`
- `media: String`
- `color_mode: String`
- `quality: U32`

## struct `JobInfo`

- `job: U32`
- `name: String`
- `printer: String`
- `state: U32`
- `line: String`
- `ink: String`

## enum `State`

- Open, Queued, Sending, Printing, Done, Failed, Canceled
