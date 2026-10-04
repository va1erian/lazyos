# ext2 journaling

**What it is.** An optional internal journal for the ext2 driver (`libs/ext2fs`),
written in the JBD2 on-disk format ext3/ext4 and `e2fsck` use. A volume with a
journal mounts through the [block cache](block-cache.md) with *metadata
journaling*: every commit logs the dirty metadata as one atomic transaction, and
the next mount replays a log a crash left behind. After a power cut the volume
needs no repair; without the journal it needs the phase-ordered crash semantics
and `Ext2::repair` of the block-cache doc.

**Status.** Implemented in the library (`libs/ext2fs/src/journal/`), the image
build (`LAZYOS_JOURNAL`, `run_demo.py --journal`, the launcher's Advanced tab)
and the kernel mount (it replays on mount and logs `ext2: <dev> replayed its
journal`). Off by default: nothing changes for an image built without it.

## Plan (what was decided, and why)

| Question | Decision | Why |
|---|---|---|
| Format | JBD2 v2 superblock, descriptor/commit/revoke blocks, journal in inode 8, `HAS_JOURNAL` + `RECOVER` flags, backup of the inode's block map in `s_jnl_blocks` | `e2fsck`, `debugfs` and Linux can read, replay and check the volume; no private format to maintain |
| What is logged | Metadata only: inodes, bitmaps, group descriptors, superblock, directory and indirect blocks | the `data=ordered` trade-off: file contents are not written twice |
| Data | Written home *before* the transaction that links it (file-data blocks are flagged in the cache) | a crash never shows a file another file's bytes, exactly the guarantee of the `Fresh` phase |
| Transactions | One per commit (`sync`, `fsync`, the 5 s flusher, unmount, a full cache), checkpointed before the commit returns; the log is empty between commits | the cache already batches a commit's worth of changes; an always-empty log needs no revoke records, no wrap, no tail tracking |
| Frees | Applied inside the commit's transaction, no longer in a second writeback | atomic, so detach and free cannot reach the disk apart |
| Barriers | No-ops on a journaled volume | every order a barrier promised holds trivially inside one transaction; a rename is atomic, not just "under at least one name" |
| Checksums, 64-bit, external journals, fast commit | Refused at mount (`NotSupported`) | not needed; refusing beats guessing |
| Opt in | `Ext2::add_journal(blocks)` on a formatted volume (also adds one in place to an existing image); a journaled volume uses it through `open_cached` (`CacheConfig::journal`, default true) | like `tune2fs -j`; the OS volume keeps its UUID and contents |

## How a commit works

`cache/journaled.rs`, `journal/commit.rs`. For a cache with a journal,
`BlockCache::flush` does:

1. **Data.** Every dirty block flagged as file data is written to its home
   block, in runs.
2. **Log.** The remaining dirty blocks (the transaction) are written to the log
   as descriptor blocks (a tag per block, the first carrying the volume UUID)
   each followed by the blocks it names; a block that begins with the journal
   magic is logged with its first word zeroed and the *escape* flag. The journal
   superblock is written in the same batch with `s_start` = the first log block
   and `s_sequence` = this transaction. A device flush ends the step: the data
   of step 1 and the log are durable.
3. **Commit.** The commit block (same transaction id) is written and flushed.
   From here a crash replays the transaction.
4. **Checkpoint.** The logged blocks are written to their home locations and
   flushed.
5. **Empty.** The journal superblock gets `s_start = 0` and the next
   transaction id, and is flushed.

Because the log is empty (`s_start == 0`) when step 2 begins, the superblock
that arms it can share the batch with the log: until the commit block lands,
replay finds old blocks with another id or a descriptor run with no commit, and
applies nothing. A transaction larger than the log (more than
`Journal::max_data_blocks` blocks) is split into several; each is atomic, the
whole is then no longer. A journal write or checkpoint error *aborts* the
journal (jbd2's rule): the cache keeps its blocks dirty, later commits fail,
and the next mount replays whatever was committed.

Inside a transaction the phases of `cache/roles.rs` do not matter (replay is
all or nothing); they still order the writeback of a volume without a journal.

### Making room mid-operation

A write that needs a page when the dirty limit is hit writes back **data
only** (`BlockCache::relieve`): metadata waits for the volume's next commit, so
an operation's changes land together. Only when every page holds metadata does
the cache commit mid-operation; that transaction then ends inside an operation,
which is what an unjournaled volume always risks. A cache of a few dozen pages
or more never meets it (`a_tiny_cache_still_commits_consistently` runs four
and still ends clean).

## Mount and recovery

`Ext2::open` (`journal/replay.rs`, before any cache exists):

1. a volume with `HAS_JOURNAL` must name inode 8 and no external journal, and
   its journal superblock must agree with the volume (block size, length);
   `RECOVER` is an accepted incompatible flag only with a journal;
2. if the volume flags `RECOVER` or the log is armed (`s_start != 0`), the log
   is walked three times (as jbd2): find the end of the committed
   transactions, collect revoke records, copy each logged block that was not
   revoked later to its home. Every loop is bounded by the journal's length
   and every home block is range-checked (and may not be a journal block), so a
   hostile log cannot hang or write outside the volume;
3. the log is emptied, then `RECOVER` is cleared and the volume marked valid
   (the log restored a state the driver committed), each step flushed.
   [`Ext2::journal_recovered`] reports that a replay happened;
4. a device that cannot be written is refused (`NotSupported`) when a replay is
   needed: showing the volume without it would show stale metadata.

While a cached journaled volume is dirty it carries `RECOVER` and a cleared
valid bit (the existing "dirty first" rule); a clean sync clears both.

## Tools

- `Ext2::add_journal(blocks)`: inode 8, the log's blocks, the journal
  superblock (UUID, one user), `s_journal_inum`, the `s_jnl_blocks` backup.
  `blocks` is 64 to a quarter of the volume (Linux wants 1024 or more).
- Image build: `LAZYOS_JOURNAL=1` (16 MiB at 4 KiB blocks) or a block count;
  `python tools/run_demo.py --journal [BLOCKS]`; the launcher's Advanced tab
  ("ext2 journal on the OS volume"). A new volume gets the journal before it is
  populated; an in-place update adds one to an image that has none and leaves an
  existing journal as it is (no resizing).
- The independent checker (`check::fsck`) claims the journal inode's blocks,
  and reports an image whose log still holds work or whose `RECOVER` flag is
  set ("journal needs recovery"), as `e2fsck` would before replaying. The
  repair (`Ext2::repair`) understands the journal inode.

## Tests

Host (`cargo test -p ext2fs journal`, `libs/ext2fs/src/tests/journal.rs`):

- `every_crash_point_replays_to_a_clean_volume`: random workloads over 1/2/4 KiB
  blocks and caches of 64-256 blocks, a power cut at random prefixes of every
  device write the cache issued (log, commit, checkpoint, markers); after the
  mount's replay the independent checker must find **nothing**, and no file may
  show another file's bytes. Fails if replay is disabled.
- `a_committed_transaction_is_replayed_and_an_unfinished_one_is_not`: every cut
  inside a commit; the new directory appears exactly when the commit block
  landed, never half.
- `a_block_that_looks_like_the_journal_magic_is_escaped`,
  `a_hostile_log_is_refused_or_ignored_never_a_panic` (noise and plausible
  headers in the log), `a_failing_disk_never_leaves_a_volume_that_needs_more_than_a_replay`
  (the disk dies after every n-th sector), `a_journaled_volume_on_a_read_only_device_with_a_pending_log_is_refused`,
  creation and refusal cases, round trips, and the tiny-cache case.
- Build support (`cargo test -p build-support-tests journal`): the
  `LAZYOS_JOURNAL` parser, and an image that gets a journal in place and then
  updates through it, fsck-clean.
- Launcher: `tools/lazygui/test_catalog_apps.py` (`JournalTests`),
  `tools/test_run_demo.py`.

## Limits and non-goals

- The journal protects metadata, not file contents: data written since the last
  commit is lost in a crash (the same loss window as before), and an overwrite
  in place can be torn at a block boundary (ordered mode, as ext3).
- Commits are per cache commit, not per system call; `fsync` is still the way
  to ask for durability.
- No commit-time clock (the commit block's timestamp is 0), no checksums, no
  asynchronous commit, no live resize, no external journal.
- A mid-operation commit (above) and a transaction bigger than the log are the
  two ways a crash can still show a half-done operation; both leave what
  `Ext2::repair` handles.
