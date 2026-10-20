# ts_r_testing

Test support for every crate.

- `cap_memory!()` installs `CappedAlloc` as the global allocator: the process aborts once live heap
  memory passes the cap (default 4096 MB, override with `TS_R_MEM_CAP_MB`). Put it in every test,
  example or harness binary that runs generated input (fuzzing, mutation) or real-world corpora.
- Unsafe is allowed here only for the `GlobalAlloc` impl and the Windows Job Object module
  (`src/job.rs`, `#[allow(unsafe_code)]` on those two items, not the crate); every block has a
  `SAFETY` comment. `job.rs` needs it because Job Objects are only reachable through Win32 FFI.
- The cap counts heap allocations only. Use `scripts/limit.sh` for whole commands, child processes
  and anything not written in Rust.
- Tests that spawn `node` or our binaries use `limited(program, mem_mb, time_s)`, never
  `scripts/limit.sh` or `bash` directly. It returns a `Limited` (configured like a `Command`:
  `arg`, `args`, `env`, `current_dir`, stdio) that must be run with its own `output()` or
  `status()`; it never hands out the inner `Command`, since running that would skip the
  Windows limits. Kills report `EXIT_MEMORY_LIMIT` (137) and `EXIT_TIME_LIMIT` (124) on both:
  - Unix: wraps `scripts/limit.sh` (resident memory of the process group); unchanged.
  - Windows: a Job Object per run (`src/job.rs`): `JOB_OBJECT_LIMIT_JOB_MEMORY` caps the
    committed memory of the whole tree, `KILL_ON_JOB_CLOSE` plus a terminate after the child
    exits kills what it left running, and the deadline ends in `TerminateJobObject`. Memory kills
    are detected from `JOB_OBJECT_MSG_JOB_MEMORY_LIMIT` on the job's completion port, or a failed
    run whose peak commit came within 1/16 of the cap. The child is assigned right after
    `spawn` (std cannot spawn suspended), so for that instant it runs unconfined and a process it
    starts then escapes the job; everything it starts later inherits the job.
- Tests that compare paths with the tools' output canonicalize with `canonical(path)`: on
  Windows `Path::canonicalize` returns a verbatim `\\?\C:\...` path that the tools never print.

## Dependencies

- `windows-sys` (Windows only; `Win32_Foundation`, `Win32_Security`, `Win32_System_IO`,
  `Win32_System_JobObjects`, `Win32_System_Threading`): the Job Object, completion port and
  handle calls `limited` needs on Windows. Already in the lockfile through other crates.
