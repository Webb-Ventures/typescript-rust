# ts_r_build

`ts-r-build`: the esbuild-compatible command line over `ts_r_bundler`. Root rules:
[../../AGENTS.md](../../AGENTS.md). Design:
[../../docs/design/0008-bundler.md](../../docs/design/0008-bundler.md); bundler:
[../ts_r_bundler/AGENTS.md](../ts_r_bundler/AGENTS.md).

## Shape

- `main.rs`: parse, build once (or watch), print messages esbuild-style to stderr, write the
  metafile, print the output size table. Without `--outfile`/`--outdir` the bundle goes to
  stdout, like esbuild.
- `args.rs`: esbuild flag syntax (`--flag`, `--flag=value`, `--flag:key=value`,
  `name=path` entry points). Unknown flags are errors. `--help` lists every supported flag.
  Our own additions: `--no-cache` (skip the persistent `ts_r_cache` namespace `build`) and
  `--threads=N`.
- `watch.rs`: `--watch` keeps one `BuildContext` alive and rebuilds on file events in the
  directories of the last build's inputs (non-recursive) plus the working directory, and in
  the directories glob `require`/`import()` patterns listed (`BuildResult::watch_dirs`),
  where any added or removed entry counts, whatever its extension.
- `parse<T>` (record 0007): builds read through `ts_r_guard::vfs::GuardFs`, which expands
  files importing `@ts-r.dev/guard` as they are read. Bundles import one virtual module per
  validated declaration (`<working dir>/.ts-r-guard/...`, shared by the bundle and
  tree-shaken); unbundled output inlines validators per file. Guard errors (TSR1501, ...)
  become build errors after the build (outputs are deleted). In `--watch`, any source change
  also re-reads the files with `parse<T>` calls and the validator modules. `[sound.guard]`
  comes from `ts_r_sound::config::guard_options` for `--tsconfig` or the nearest tsconfig,
  read at startup (TSR0020 problems are printed).

## Invariants

- Flag names and meanings match esbuild 0.28.2; when we cannot honour a flag we reject it
  rather than silently ignoring it (a few output-cosmetic flags such as `--color` are
  accepted and ignored; `--charset=utf8` keeps non-ASCII in CSS output, JavaScript output
  keeps it either way; `--jsx-side-effects` is accepted because JSX is never removed).
  `NODE_PATH` is read like esbuild's CLI reads it. An entry `out/name=in/file.js` names its
  output, unless the whole argument is an existing file.
- No JavaScript config file, no plugins.
- Exit code 1 on any error message, 0 otherwise.

## Testing

`cargo test -p ts_r_build` (argument parsing; `tests/guard.rs`: `parse<T>` bundles run in Node
like `ts-r-run`, shared validator modules, unbundled output, errors, and the minified size
gate: runtime 4,177 B, runtime + ten API types 8,220 B). End-to-end behaviour is tested through the
library in `ts_r_bundler` and against esbuild with the oracle in
[../../bench/esbuild-oracle/README.md](../../bench/esbuild-oracle/README.md).

## Dependencies

`ts_r_bundler`, `ts_r_config`, `ts_r_guard` (`parse<T>` call sites), `ts_r_sound` (only
`config::guard_options`). External: `rustc-hash` (hash maps), `notify` (file-system
events for `--watch`, same version and features as `ts_r_run` and `ts_r_hmr`).
Dev: `ts_r_testing`.
