# `sys::print_`

The Rhai module for the Messenger interface `os.lazy.print.v1`. Wire-level reference: [os.lazy.print.v1](../../idl/os.lazy.print.v1.md).

The print spooler, `printd` (docs/printing-plan.md P6).

An app opens a job, writes its whole document with `Write` and queues it
with `Close`; only a closed job is sent to its printer, so a printer never
gets half a request because the app quit. A job answers only to the uid
that opened it (and root): the uid is the kernel-stamped sender, never a
wire field, and anyone else is told the job does not exist. Failures are
the shared structured error field, `EINVAL` with one line for the user
(`There is no such print job`, `The print queue is full; ...`).

## Functions

### `open(printer, user, ticket)`

`Open(printer: String, user: String, ticket: Ticket) -> (job: U32)`

Open a job for the printer at `printer` (`192.168.1.89`,
`host:port` or an `ipp://` URI). `user` is the `requesting-user-name`
the printer shows, a label only.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `printer` | `String` | string |
| `user` | `String` | string |
| `ticket` | `Ticket` | map (`Ticket`) |

Returns `job`: `U32` (int).

```rhai
let result = sys::print_::open(printer, user, ticket);
```

### `write(job, bytes)`

`Write(job: U32, bytes: Bytes) -> ()`

Append `bytes` to an open job's document (at most 256 KiB a call).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `job` | `U32` | int |
| `bytes` | `Bytes` | blob |

Returns `()`.

```rhai
let result = sys::print_::write(job, bytes);
```

### `close(job)`

`Close(job: U32) -> ()`

The document is complete: queue the job for its printer.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `job` | `U32` | int |

Returns `()`.

```rhai
let result = sys::print_::close(job);
```

### `cancel(job)`

`Cancel(job: U32) -> ()`

Stop a job wherever it is; one the printer has is canceled there.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `job` | `U32` | int |

Returns `()`.

```rhai
let result = sys::print_::cancel(job);
```

### `status(job)`

`Status(job: U32) -> (info: JobInfo)`

What a job is doing.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `job` | `U32` | int |

Returns `info`: `JobInfo` (map (`JobInfo`)).

```rhai
let result = sys::print_::status(job);
```

### `jobs()`

`Jobs() -> (jobs: Array<JobInfo>)`

The caller's jobs (every job for root), oldest first.

Returns `jobs`: `Array<JobInfo>` (array of map (`JobInfo`)).

```rhai
let result = sys::print_::jobs();
```

## Types

### `Ticket`

The print dialog's choices, as IPP job attributes.

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `format` | `String` | string |
| `copies` | `U32` | int |
| `media` | `String` | string |
| `color_mode` | `String` | string |
| `quality` | `U32` | int |

```rhai
let value = sys::print_::new_ticket();
```

### `JobInfo`

`job-name`: the document's title.
`document-format`, e.g. `image/pwg-raster`.
`copies`, 1 to 99; 0 leaves it to the printer.
`media`, a PWG name such as `iso_a4_210x297mm`; empty for the
printer's default.
`print-color-mode`: `color`, `monochrome`, or empty.
`print-quality`: 3 draft, 4 normal, 5 high; 0 for the default.
One job, for the app that printed it.

| Name | IDL type | Script value |
|---|---|---|
| `job` | `U32` | int |
| `name` | `String` | string |
| `printer` | `String` | string |
| `state` | `U32` | int |
| `line` | `String` | string |
| `ink` | `String` | string |

```rhai
let value = sys::print_::new_job_info();
```

### `State`

A string; the constants of this module are:

- `STATE_OPEN` = `"Open"`
- `STATE_QUEUED` = `"Queued"`
- `STATE_SENDING` = `"Sending"`
- `STATE_PRINTING` = `"Printing"`
- `STATE_DONE` = `"Done"`
- `STATE_FAILED` = `"Failed"`
- `STATE_CANCELED` = `"Canceled"`

`STATE` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.print.v1 on the wire](../../idl/os.lazy.print.v1.md)
