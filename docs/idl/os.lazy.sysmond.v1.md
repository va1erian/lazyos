# `os.lazy.sysmond.v1`

Interface id: `0x5cd4605eb47c3d8f`

The system monitor service (issue #144): one live system-stats snapshot.

The snapshot is the kernel's fixed-layout `sysinfo` block returned as raw
bytes, so a client decodes the same block `top` reads directly from the
kernel. Failures are returned as a structured error field (errno-style
code, friendly text), not as a typed reply.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Snapshot | 1312231341 | sync | `() -> (data: Bytes)` |
