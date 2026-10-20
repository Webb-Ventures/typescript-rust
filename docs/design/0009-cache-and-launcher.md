# 0009: Content-addressed cache and the `ts-r` launcher

Status: Accepted 2026-10-18 by Riley Webb
Owners: `ts_r_cache` (on-disk cache, cache root, read-only inspection), `ts_r_cli` (the `ts-r`
launcher); consumers: every tool that caches (`ts_r_hmr`, `ts_r_dev`, `ts_r_bundler`,
`ts_r_run`, `ts_r_lint`, `ts_r_decls`, `ts_r_history`), `ts_r_lsp` (cache root only),
`npm/ts-r` (mirrors the root lookup)

This record was written after the code, from the code and the two crates' `AGENTS.md`. It
describes what exists; where it and the code disagree, the code was checked and the
disagreement is listed under "Known gaps".

## Context

- Each tool must give its full speed win when used alone, and several tools (an editor daemon,
  a dev server, `ts-r ci`, a watch build) often run at once on one machine. So the persistent
  cache is one shared store that many processes and threads read and write together.
- The plan's budget for a warm run where nothing changed is under 100 ms for 10k files. A first
  design with one file per entry measured 177 ms warm and 1.35 s cold for 10k × 4 KiB entries on
  APFS (opening a file costs about 10-20 µs on macOS). Small entries therefore have to be
  packed together.
- On Windows, byte-range locks are mandatory (a locked file can't be read or mapped by anyone
  else), and a file deleted while another process has it open or mapped can stay "delete
  pending" under its name. Opening it then fails with `ERROR_ACCESS_DENIED`. The cache runs on
  Windows in CI, so both cases are part of the design.
- The `ts-r` launcher is a thin dispatcher with no compiler code. `ts-r check` should behave
  exactly like running `ts-r-check`, start in well under 5 ms, and let a project pin a
  ts-r version (like rustup or Volta).

## Decision

### Cache root

`ts_r_cache::cache_root()` resolves the root (first match wins). It does not create it.

1. `TS_R_CACHE_DIR` (used as is, if not empty)
2. `$XDG_CACHE_HOME/ts-r`, only when the variable is an absolute path
3. Windows: `%LOCALAPPDATA%\ts-r`
4. `$HOME/.cache/ts-r` (macOS too: the plan documents `~/.cache/ts-r` everywhere)
5. Windows: `%USERPROFILE%\.cache\ts-r`
6. `<temp dir>/ts-r-cache` as a last resort

`cache_root_with_source()` also returns the reason, which `ts-r doctor` prints. Nothing else
works out the root itself. `npm/ts-r/bin/ts-r.js` keeps a JavaScript copy for its fallback and
must stay in sync with this one.

Under the root:

```
<root>/
  bin/<version>/ts-r-<tool>     launcher's per-version binaries (bin_dir); not managed by gc
  lsp/                          ts_r_lsp's daemon endpoints and parity results (record 0011);
                                plain files, not a cache namespace
  <namespace>/v<format>/        one Cache handle
    packs/<id>.open | <id>.pack small entries, back to back, 128-byte aligned
    packs/<id>.lock             held by the segment's writer, deleted when it is done
    objects/<2 hex>/<62 hex>    large entries, one file each
    index                       sorted snapshot
    journal                     append-only records since the snapshot
    index.lock, gc.lock         lock-only files
    tmp/                        temp files (<pid>-<n>-<nanos>.tmp)
```

### Namespaces and format versions

`Cache::open(namespace, format_version)` (or `open_in(root, ..)`, `with_options(..)`) opens
`<root>/<namespace>/v<format_version>/`, creating `objects/`, `packs/` and `tmp/`. A namespace is
1-64 characters of `[a-z0-9_-]` and is not `bin` or `tmp`; anything else is an `InvalidInput`
error. Each tool owns its namespaces and bumps its own format version whenever its payload
layout changes. Namespaces in use: `dev-transform`, `dev-deps` (`ts_r_hmr`), `dev-plugins`
(`ts_r_dev`), `build` (`ts_r_bundler`), `run` (`ts_r_run`), `lint-config` (`ts_r_lint`),
`decls` (`ts_r_decls`), `check` (`ts_r_history`).

### Keys

A `Key` is 32 bytes of BLAKE3. `KeyBuilder::new(domain)` starts a hasher with the domain string
(for example `"ts-r-dev/transform"`) and adds fields with `bytes`, `str`, `u64` and `key_field`.
Every field is written as its length (`u64`, little-endian) and then its bytes, so `("ab", "c")`
and `("a", "bc")` give different keys. `Key::of(data)` hashes a single blob. A key must cover
everything that decides the value: the tool version, the normalised options, the content, and
the keys of dependencies. `to_hex`/`from_hex` use 64 hex characters.

### Entry format

Every entry, loose or packed, is a 128-byte header followed by the payload:

| offset | size | field |
|---|---|---|
| 0 | 4 | magic `TSRC` |
| 4 | 4 | entry layout version (`ENTRY_LAYOUT` = 1), `u32` LE |
| 8 | 4 | namespace format version, `u32` LE |
| 12 | 4 | reserved, zero |
| 16 | 8 | payload length, `u64` LE |
| 24 | 32 | key |
| 56 | 32 | BLAKE3 of the payload |
| 88 | 40 | zero |

Because the header is 128 bytes and entries in segments start at multiples of 128, a payload
read from a memory map starts 128-byte aligned, so callers can read index-based data (`u32`
arrays) in place.

### Loose entries and pack segments

- An entry (header + payload) smaller than `pack_threshold` (default 1 MiB) is **packed**: it
  is appended to the handle's own segment, `packs/<16 hex id>.open`. Before the segment
  exists, the handle creates and locks `packs/<id>.lock` and holds it while appending. A writer
  starts a new segment once its current one reaches 64 MiB. On `flush` (also run on drop) the
  order is: write the data out, rename `.open` to `.pack`, append the journal records, then
  unlock and delete the lock file. From then on the `.pack` is never written again, and other
  processes see the entries.
- A larger entry is **loose**: written to `tmp/`, then renamed to `objects/<2 hex>/<62 hex>`. It is
  visible at once. When read, a loose entry of at least `mmap_threshold` (default 64 KiB) is
  memory-mapped; a smaller one is read into the heap.
- Reads return a `Blob` (`Deref<[u8]>`). A handle maps each flushed segment once and serves its
  entries as slices of the map. `Blob::is_mapped` tells whether the data is 128-byte aligned.

### Index and journal

- `index` is an immutable sorted snapshot. Its 64-byte header holds magic `TSRI`, layout 2, the
  record count at offset 8 and the BLAKE3 of the records at 24..56. It is followed by 64-byte
  records sorted strictly by key: key (32 bytes), size, `last_used` (Unix seconds), segment id
  (0 = loose) and offset (each `u64` LE). Readers map it without locking and binary-search it.
  A snapshot with the wrong length, checksum or order counts as empty.
- `journal` is append-only and made of 80-byte records: key, the same 32 bytes of record data,
  a kind (1 put, 2 touch, 3 remove), zero padding, and the first 4 bytes of the BLAKE3 of the
  first 76 bytes. A writer appends its whole batch with one write while holding `index.lock`
  **shared**, so appends never block each other. On decode, records that fail the check are
  skipped, and decoding resynchronises on the next valid record if a torn append has shifted
  the 80-byte grid.
- A handle's view is the snapshot plus the folded journal plus its own unflushed changes. On a
  miss it reloads the view at most every 50 ms to pick up other processes' writes.
- Packed entries can only be found through the index. Loose entries can also be found by path,
  so for them the index only feeds LRU order and `usage()`.

### Compaction and gc

- `flush` compacts when the journal has reached `compact_journal_bytes` (default 4096 records).
  `compact()` try-locks `gc.lock` (if it is busy, it returns `false` and does nothing), takes
  `index.lock` exclusively, folds the journal into a new snapshot, writes it to `tmp/`, renames it
  over `index`, and truncates the journal. Folding is idempotent, so a crash between the rename
  and the truncate does no harm.
- `gc(max_bytes)` try-locks `gc.lock` (busy: `GcReport::skipped_busy`) and flushes its own
  writes. It then works in three phases:
  1. Under the exclusive `index.lock`, it reads the snapshot and journal and notes whether
     either is damaged.
  2. Without the lock, it scans loose files and segments. It deletes files with invalid names
     and loose files whose key has since been written packed. An unlocked `.open` segment (a
     writer that crashed) has its intact entries recovered and is renamed to `.pack`. An
     unreferenced `.pack` is deleted, unless the index was damaged, in which case it is
     recovered too. A locked segment is never touched. Next, gc evicts least-recently-used
     entries (oldest first, ties broken by key) until the namespace is under `max_bytes`. It
     rewrites segments that are less than half live, and merges segments under 4 MiB once
     there are at least 4 of them. Temp files older than an hour are removed, and unlocked
     lock files whose segment is published are removed (if the segment is gone, only after
     10 minutes).
  3. Under the exclusive `index.lock` again, it applies the journal records appended since
     phase 1, writes the new snapshot and deletes the segments it emptied.
- `maybe_gc(max_bytes)` runs `gc` down to 80% of `max_bytes` when `usage()` is over it.
  `DEFAULT_MAX_BYTES` is 2 GiB per namespace. Neither runs automatically.

### Reads, corruption and errors

- Every read checks the whole entry: header fields, length, key, and the payload's BLAKE3
  (about 1 µs per 4 KiB). Any mismatch, including a format-version mismatch, counts as
  corruption. The read is a miss, and the entry is dropped: a corrupt loose file is deleted
  (only if it is still the same inode), and a corrupt packed entry gets a journal `remove`.
- I/O errors are misses, counted in `Stats::io_errors`, and the text of the first one is kept
  for `Cache::first_io_error`. A file that is gone (see Windows below) is not an I/O error.
- A hit records a touch only if the entry's `last_used` is more than
  `TOUCH_GRANULARITY_SECS` (1 hour) old. So a warm run that reads everything writes nothing, and
  LRU order is accurate only to the hour.
- No fsync. After a power loss, checksums turn torn data into misses.

### Multi-process contract

Any number of threads and processes may open the same namespace. Readers take no file locks.
Each pack segment has exactly one writer, which holds that segment's lock file. Journal
appends hold `index.lock` shared. Compaction and gc hold it exclusively, and only for the two
short moments when the index is read and replaced. Maintenance is serialized by `gc.lock`
(try-lock: a busy lock means skip, never wait). Files are immutable once visible, and gc only
unlinks. The only `unsafe` is the three `memmap2::Mmap::map` calls (index snapshot, loose
entry, pack segment), and their soundness rests on that immutability. A process outside ts-r
that truncates a mapped file is outside the contract.

### Windows: delete pending and sharing violations

All Windows-specific handling is in `src/os.rs`. On Unix it does nothing.

- `os::is_gone(e)` is `NotFound`, or on Windows `ERROR_ACCESS_DENIED` (5) or
  `ERROR_DELETE_PENDING` (303). Segments stay mapped for a reader's whole life, so a segment
  that gc deleted can linger as delete pending for a long time. Reads treat it as gone: a
  miss, and for a segment, a refresh of the view to find its new location. Inside the cache
  directory, an access-denied error on open counts as a miss, not an `io_errors`, because std
  does not expose the NT status that would tell it apart from a real permission error.
- A loose `put` whose rename runs into a delete-pending file drops the value, as if it had been
  stored and then evicted. If the key already has a value, that value is kept.
- `os::retry_busy` retries opens and renames that fail with `ERROR_SHARING_VIOLATION` (32) or
  `ERROR_LOCK_VIOLATION` (33) 5 times, with delays doubling from 1 ms (about 31 ms in total).
- Locks are on lock files, never on data files, because a locked segment could not be read,
  copied or mapped. Files are replaced by renaming a new file over them (std's rename keeps
  existing maps valid on Windows), never by rewriting them in place.

### The `ts-r` launcher

| Command | Does |
|---|---|
| `ts-r run\|dev\|build\|lint\|check\|lsp [args]` | runs `ts-r-<tool>` with the arguments untouched |
| `ts-r ci [--skip-missing]` | `check`, `lint` and `build` in parallel, prefixed lines, timing summary |
| `ts-r doctor` | where each tool is found, cache dir, size and namespaces, nearest tsconfig/package.json |
| `ts-r -V\|--version\|version`, `-h\|--help\|help`, no arguments | launcher version, usage |

- Arguments are parsed by hand (no clap). `--version` and `--help` touch nothing on disk.
- **Version selection** (`version::select`): `TS_R_VERSION` if it is not blank, then the nearest
  `.ts-r-version` walking up from the current directory, then the launcher's own version. A
  pin file holds the first line that is not blank or a `#` comment. A leading `v` is dropped,
  and the version must match `[0-9][A-Za-z0-9.+_-]*` (at most 64 characters, so a pin can't be
  a path). An unreadable or invalid pin is a usage error.
- **Tool lookup** (`resolve::resolve`). For the launcher's own version, it looks in the
  launcher's directory (and its symlink-resolved directory, for Homebrew and npm links), then
  in `<root>/bin/<version>/`, then on `PATH`. For a pinned **other** version, it looks only in
  `<root>/bin/<pinned>/`, because binaries found elsewhere are of unknown version. A binary is
  named `ts-r-<tool>` plus the platform's executable suffix.
- **Running**: on Unix the launcher `exec`s the tool, so signals, stdio, the TTY and the exit
  code belong to the tool. Elsewhere it spawns the tool, waits and forwards the exit code.
- **Exit codes**: the tool's own; 127 when the tool is not installed; 126 when it was found but
  could not be started; 2 for a usage error or a bad pin. `ts-r ci` exits 0 if every tool
  passed and 1 otherwise (2 for a bad argument).
- **Missing tools** report the places searched. With no pin, they also name the milestone that
  ships the tool (the table in `src/tools.rs`).
- **`ts-r ci`** resolves all three tools first. If one is missing it runs nothing (exit 1),
  unless `--skip-missing` is given. It gives each tool `TS_R_THREADS = cores / tools`, writes
  whole lines under the stdout/stderr lock with a `name |` prefix, keeps stderr on stderr, and
  passes bytes through unchanged. It forces colour in the tools (`FORCE_COLOR`,
  `CLICOLOR_FORCE`) only when it is itself on a TTY and `NO_COLOR` is unset.

## Invariants agents must keep

- Format versions never mix: the version is in the directory name and in every entry header,
  and a mismatch is a miss plus a drop. Bump the namespace's format version whenever a
  payload layout changes, and `ENTRY_LAYOUT` or the index `LAYOUT` whenever this crate's own
  formats change.
- Reads never take file locks and never panic. Every read is verified.
- Files are immutable once visible: loose entries by temp file + rename, segments by one
  locked writer and the flush order data → rename → journal → unlock. gc only unlinks.
- Locks live on lock files, never on data files.
- Windows sharing semantics are handled in `src/os.rs` and nowhere else.
- A warm run writes nothing (touch granularity of one hour).
- No timestamps or absolute paths in payloads. Index times are cache metadata, not output.
- Cache-root logic exists only in `ts_r_cache::cache_root()` (and its mirror in
  `npm/ts-r/bin/ts-r.js`, kept in sync).
- The launcher contains no compiler code, stays under 5 ms to start, uses `exec` on Unix, and
  touches the file system only when the command needs it.
- A pinned other version runs only binaries from `<root>/bin/<pinned>/`.

## Known gaps

- Nothing collects garbage automatically. No crate calls `maybe_gc` or `gc` today. There is no
  budget across namespaces, and old `v<format>` directories are never collected. There is no
  `ts-r cache gc|clean` subcommand: `doctor` only reports.
- `Stats` count for one handle only; nothing keeps hit/miss counts across processes.
- Heap blobs (small loose entries, a handle's own unflushed packed entries) are not 128-byte
  aligned. Only mapped blobs are.
- A handle whose view was loaded in the middle of a compaction can miss entries until its next
  refresh (at most every 50 ms, on a miss).
- `lsp` is not a reserved namespace name, although `ts_r_lsp` writes plain files under
  `<root>/lsp/`.
- Large entries are verified with single-threaded BLAKE3. There is no remote cache.
- The launcher does not download tools into `<root>/bin/<version>/`: the npm shim installs only
  the launcher. `ts-r ci` does not run tsc 7 beside `check` for a parity report. Windows
  launcher paths compile, but the end-to-end launcher tests (`tests/cli.rs`) are Unix-only.
- `npm/ts-r/bin/ts-r.js`'s `cacheRoot()` is not an exact mirror. It uses `os.homedir()`
  instead of `HOME`/`USERPROFILE`, and it has no temp-directory fallback.

## Alternatives

- **One file per entry** (the first design): 177 ms warm and 1.35 s cold for 10k × 4 KiB on
  APFS, which breaks the 100 ms warm budget. Replaced by pack segments for entries under 1 MiB.
- **Locking data files** (segments, the index): impossible on Windows, where locks are mandatory
  and would block readers. Locks moved to separate lock files.
- **fsync on every flush**: not done. Checksums already turn torn writes into misses, and a
  cache may lose data.
- **Touching on every read**: would make warm runs write. Touches are kept to one per hour per
  entry.
- **clap for the launcher**: rejected for the startup budget. Measured with hand-written
  parsing on an M-series Mac: `ts-r --version` 2.1 ms, `ts-r run` exec-ing a sibling `true` 3.2
  ms, about 0.5 ms of launcher overhead; 0.7 MB binary.
- **Searching `PATH` for pinned versions**: rejected, because a binary found there has an
  unknown version.

## Status

Written 2026-10-18 from the code at the top of the stack (`m7/bench-box`), after the cache and
launcher had shipped. Accepted 2026-10-18 by Riley Webb.
