# ts_r_cache

Content-addressed, versioned, memory-mapped on-disk cache in `~/.cache/ts-r`, shared by every
ts-r tool. Each tool gets its full win from it alone; several tools running at once share it
safely. Root rules: [../../AGENTS.md](../../AGENTS.md).
Design record: [0009](../../docs/design/0009-cache-and-launcher.md).

## API in one breath

`Cache::open(namespace, format_version)` → `get(&Key) -> Option<Blob>` (verified, `Deref<[u8]>`),
`put(&Key, &[u8])`, `contains`, `meta`, `remove`, `flush` (also on drop), `stats`, `usage`,
`gc(max_bytes)`, `maybe_gc(max_bytes)`, `compact`, `first_io_error` (diagnostics). Keys come from `KeyBuilder` (length-prefixed
BLAKE3 fields: tool version, options, content, ...). `cache_root()` / `bin_dir()` locate the
cache and the launcher's per-version binaries; `inspect` is read-only, for `ts-r doctor`.

## Layout

```
<root>/                         TS_R_CACHE_DIR > $XDG_CACHE_HOME/ts-r > %LOCALAPPDATA%\ts-r > ~/.cache/ts-r
  bin/<version>/ts-r-<tool>     launcher's version dir (not managed by this crate's gc)
  <namespace>/v<format>/        one Cache handle; namespaces: [a-z0-9_-]{1,64}, not bin/tmp
    packs/<id>.open | <id>.pack small entries (< 1 MiB), appended back to back, 128-aligned
    packs/<id>.lock             held (locked) by the segment's writer; deleted when it is done
    objects/<2 hex>/<62 hex>    large entries, one file each (temp + rename)
    index                       sorted snapshot: key -> size, last_used, segment, offset
    journal                     append-only 80-byte checksummed records since the snapshot
    index.lock, gc.lock, tmp/
```

Every entry (loose or packed) = 128-byte header (magic, layout, format version, payload len,
key, BLAKE3 of payload) + payload. See `src/entry.rs`, `src/index.rs`, `src/pack.rs` docs.

## Invariants

- **Versions never mix.** The format version is in the directory name *and* every entry header;
  a mismatch is treated as corruption (miss + drop). Bump `format_version` whenever a
  tool's payload layout changes. `ENTRY_LAYOUT` / index `LAYOUT` cover this crate's own formats.
- **Reads never lock and never panic.** Missing, truncated, mis-keyed or checksum-failing
  entries are misses; corrupt loose files are deleted (only if still the same inode), corrupt
  packed entries get a journal `remove`. I/O errors are misses counted in `Stats::io_errors`
  (the first is kept as text for `Cache::first_io_error`). A file being deleted is not an I/O
  error: see the Windows invariant below.
- **Windows sharing semantics are handled in `src/os.rs`, nowhere else.** A file deleted while
  another process still has it open or mapped can stay "delete pending" under its name, and
  opening it fails with `ERROR_ACCESS_DENIED`. Segments stay mapped for a reader's whole life,
  so a segment gc deletes can linger like that for a long time. Reads treat it as `NotFound`
  (`os::is_gone`: a miss, and for a segment a refresh to its new location), and a loose `put`
  whose rename hits it drops the value (as if stored and evicted). Opens and renames retry
  sharing violations for ~30 ms (`os::retry_busy`). On Unix both are no-ops.
- **Every read is verified** (BLAKE3 over the payload, ~1 µs per 4 KiB).
- **Files are immutable once visible.** Loose: temp file + rename. Packed: one writer per
  segment holding its lock; flush order is data → rename `.open`→`.pack` → journal append →
  unlock. A `.pack` is never written again, so mapping it is sound. gc only unlinks.
- **Locks live on lock files, never on data.** Windows byte-range locks are mandatory, so a
  locked segment could not be read, copied or mapped by another process. A segment writer
  locks `packs/<id>.lock` (created before the segment, deleted before it is unlocked);
  `index.lock` and `gc.lock` are lock-only files too. gc treats a segment without a lock file
  as locked only if the `.open` file itself is (writers from before lock files). Unlocked lock
  files are deleted by gc once their segment is `.pack` or gone (gone: only after
  `ORPHAN_AGE`).
- **Nothing is rewritten in place while it may be mapped.** Snapshots and loose entries are
  replaced by renaming a new file over them (std's rename keeps existing maps valid on Windows
  too); the journal is read, never mapped, so truncating it is fine. A test that rewrites a
  mapped file in place must drop the map first (Windows: error 1224).
- **Journal appends hold `index.lock` shared; compaction/gc hold it exclusive** (briefly), then
  write the snapshot via rename and truncate the journal, so no record is lost. Folding is
  idempotent. Maintenance (gc, compaction) is serialized by `gc.lock` (try-lock; busy = skip).
- **gc never touches a locked segment.** Unlocked `.open` = crashed writer: its intact entries are
  recovered (entries are self-describing). Unreferenced `.pack` = all removed/evicted → deleted,
  unless the index was damaged, in which case it is recovered too.
- **Warm runs write nothing**: reads only record a touch if `last_used` is older than
  `TOUCH_GRANULARITY_SECS` (1 h). LRU order is therefore hour-granular.
- **No timestamps or absolute paths in payloads** (the root determinism rule); index
  bookkeeping times are cache metadata, not output.
- No fsync: after a power loss, checksums turn torn data into misses.

## unsafe

`unsafe_code` is allowed only on the three `memmap2::Mmap::map` calls (index snapshot, loose
entry, pack segment), each with a `// SAFETY:` comment. Soundness rests on the immutability
invariant above. A process *outside* ts-r truncating a mapped file could raise SIGBUS; that is
outside the contract (same as every mmap-based cache).

## Dependencies

- `blake3`: fast hashing for keys and checksums (approved list in the root AGENTS.md).
- `memmap2`: portable mmap (approved list). File locks use std (`File::lock*`, Rust 1.89+).

## Performance (Apple M-series, release, `cargo run --release -p ts_r_cache --example bench`)

| 10k entries | 1 KiB | 4 KiB | 32 KiB |
|---|---|---|---|
| cold put all + flush | 25 ms | 47–56 ms | 349 ms |
| warm open + get all, 1 thread | 14 ms | 27 ms | 159 ms (BLAKE3-bound) |
| warm open + get all, 4 threads | 6–11 ms | 8.7 ms | 45 ms |
| open + contains all (index only) | | 1.1 ms | |

The plan's "warm, nothing changed: <100 ms for 10k" holds with room to spare. One file per
entry (the first design) measured 177 ms warm / 1.35 s cold for 10k × 4 KiB on APFS, which is
why small entries are packed.

## Testing

- `cargo test -p ts_r_cache`: unit tests per module; `tests/cache.rs` (round trips, isolation,
  loose and packed corruption, index loss + rebuild, crash recovery, LRU, segment rewrite,
  stale readers across a rewrite, compaction, deleted cache dir, threads);
  `tests/multiprocess.rs` (6 reader/writer processes + 1 gc process on one namespace for 1.5 s,
  asserting no wrong/partial reads, no I/O errors (a failure prints `first_io_error`) and that
  eviction and rewrites really happened).
- Benchmark: `cargo run --release -p ts_r_cache --example bench -- [entries] [bytes]`.
- The example and both test binaries generate their input, so they install
  `ts_r_testing::cap_memory!()` (dev-dependency).

## Gaps

- No per-root budget across namespaces and no automatic gc; tools call `maybe_gc` (default
  budget `DEFAULT_MAX_BYTES` = 2 GiB per namespace). Stale `v<old>` dirs are never collected.
- `Stats` are per handle; nothing persists hit/miss counts across processes.
- Heap blobs (small loose entries, own unflushed packed entries) are not 128-aligned; only
  mapped blobs are. Callers needing aligned data in place should check `Blob::is_mapped`.
- A handle's index view refreshes on misses (rate-limited to every 50 ms); a reader that
  loaded its view mid-compaction can miss entries until then.
- Windows (tested in CI): renaming open segments and deleting mapped files rely on std's
  `FILE_SHARE_DELETE` and POSIX-semantics rename; files gc cannot delete yet (or that linger
  as delete pending) are retried by the next gc. Telling "delete pending" from a real
  permission error needs the NT status, which std does not expose, so inside the cache dir an
  `ERROR_ACCESS_DENIED` on open counts as a miss, not an `io_errors`.
- Large-entry verification is single-threaded BLAKE3; no remote cache yet.
