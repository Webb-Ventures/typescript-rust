# ts_r_watch

Native file watching with debouncing tuned for editor save patterns, moved out of
`ts_r_hmr` (design record [0012](../../docs/design/0012-tsc-emit.md), phase E4) so that
`ts-r-check --watch` does not link the dev server. `ts_r_hmr` re-exports it as
`ts_r_hmr::watch`. Root rules: [../../AGENTS.md](../../AGENTS.md).

| Item | What it does |
|---|---|
| `Debouncer` | Pure: collects raw events, decides when and what to flush (unit-tested with a fake file system) |
| `watch` | Watches a root recursively and calls a sink with each settled `ChangeBatch`; ignores `node_modules`, VCS directories and OS litter (`ignored`) |
| `watch_filtered` | `watch` with the caller's ignore rule (ts-r-check watches `node_modules`: an installed package changes resolution) |
| `WatchAdder` | Adds directories (recursively or not) to a running watcher |

## Invariants

- **Atomic saves, double writes and delete-then-recreate become one change** (see the
  module docs): a burst settles after `quiet` (3 ms) or `max_wait` (30 ms); an empty or
  missing file gets `grace` (40 ms) before it is reported.
- **A file created and removed within one burst is a temp file, unless it was reported.**
  FSEvents keeps a path's flags for a while, so deleting a file created a moment ago arrives
  as create + remove; the debouncer remembers the paths it reported as present (one entry per
  file changed while watched) and reports those as removed. A new file that is still empty
  when it settles is reported as created, not modified.
- **Files untouched since the watch started are not changes.** FSEvents (macOS) replays writes
  made just before the stream started; during `STARTUP_WINDOW` (10 s) a settled change whose
  file's mtime predates the start is dropped. Removals and mtimes without a sub-second part
  (coarse file systems) are always kept.
- **macOS fast path**: files that changed recently (an LRU of `FAST_PATH_FILES`, 64) are also
  watched with kqueue and re-armed after every change; both sources feed the same debouncer.
- Dropping the `FileWatcher` stops the watch thread (it owns the native watcher) within
  250 ms. Adding a directory is a channel send, never a lock on the hot path.
- Sinks run on the watch thread: they must hand work off (a channel), not compile inline.

## Dependencies

- `notify`: cross-platform native file watching (FSEvents/inotify/ReadDirectoryChangesW) is
  real work and platform-specific; it is the standard crate (approved list).
- `kqueue` (macOS only): the safe kqueue wrapper notify itself uses; needed because notify can't
  run its FSEvents and kqueue backends side by side. Bounded to `FAST_PATH_FILES` (64) fds.

## Tests

`cargo test -p ts_r_watch`: the debouncer's unit tests and one real-watcher test (files
written before the watch starts are not reported). The dev server's end-to-end watch tests
stay in `ts_r_hmr` and `ts_r_dev`.
