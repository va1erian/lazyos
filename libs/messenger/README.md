# libmessenger

The Messenger parcel codec, shared by the LazyOS kernel and userspace. The wire
format is parcel version 2 (`docs/messenger-core-plan.md` 3.1, [`docs/messenger.md`](../../docs/messenger.md) section 4);
the philosophy is "skip what you do not know, and never panic on malformed
input".

## Layout

- `Header` / `Parcel` / `Object` / `Buffer` — the framed message (parcel
  version 2: header, TLV body, object list; `docs/messenger-core-plan.md` 3).
- `Encoder` / `Decoder` / `Field` — the TLV body.
- `Error` — structured, friendly decode failures.

## Tests

The crate is `no_std` for the OS build and uses the host standard library for
tests, so run them from the repository root:

```bash
cargo test -p libmessenger
```

That runs the round-trip suite, the limits suite, and `fuzz_decode_never_panics`
(one million mutated/truncated/random buffers, all of which must decode to
`Ok`/`Err` without panicking) and the seeded fuzz of `fuzz::run`, the entry point
shared with the cargo-fuzz target (`fuzz/fuzz_targets/messenger.rs`; seeds in
`fuzz/seeds/messenger`, written by `fuzz/gen_corpus.py`). CI runs the same command (`.github/workflows/messenger.yml`).

The kernel depends on this crate as a path dependency so it builds for
`x86_64-unknown-none` on every OS build; there is no separate cross build step.
