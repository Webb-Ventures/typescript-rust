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
| `WatchAdder` | Adds directories (recursively or not) to a running watcher; `known` names a file the consumer read |

## Invariants

- **Atomic saves, double writes and delete-then-recreate become one change** (see the
  module docs): a burst settles after `quiet` (3 ms) or `max_wait` (30 ms); an empty or
  missing file gets `grace` (40 ms) before it is reported. A file found missing that is back
  after the grace is reported as created (deleted and restored), never as modified: the dev
  server retries modules that failed to resolve an import on a creation, as Vite does on
  chokidar's `add` (the Vite oracle's hmr "should hmr when file is deleted and restored").
- **A file created and removed within one burst is a temp file, unless it is known.**
  FSEvents keeps a path's flags for a while, so deleting a file created a moment ago arrives
  as create + remove; the debouncer remembers the paths it reported as present (one entry per
  file changed while watched) and the paths the consumer says it read (`WatchAdder::known`:
  the dev server sends each module file once, when it first serves it), and reports those as
  removed. Without the second set, deleting a file that existed before the watch and was never
  edited during it was dropped about half the time (the Vite oracle's hmr "delete file should
  not break hmr"). Probing when the event arrives can't tell the two apart: the file is already
  gone by then in both cases. A new file that is still empty when it settles is reported as
  created, not modified.
- **A known file that is still there was modified, whatever its flags.** The same sticky flags
  mark edits of a recently created (or copied-in) file as creations; a consumer that treats a
  creation specially (the dev server retries modules that failed to resolve an import, as
  Vite does on chokidar's `add`) then did that work on every edit, and the Vite oracle's hmr
  page reloaded on edits. Once its removal is reported a path is unknown again, so restoring
  a deleted file is a creation.
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
