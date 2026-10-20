//! Project references, incremental builds and `tsc --build` for ts-r-check (design record
//! 0012, phase E3).
//!
//! - [`buildinfo`]: ts-r's own `.tsbuildinfo` at tsc's path.
//! - [`incremental`]: which outputs a build writes, and the buildinfo it leaves.
//! - [`references`]: the resolver's source-to-output map for referenced projects, and the
//!   checks of references (TS6306, TS6310) and composite programs (TS6307).
//! - [`graph`], [`status`], [`build`]: the `--build` driver: build order, up-to-date
//!   statuses and their messages, the build flags, parallel builders.
//! - [`clock`]: local clock times for status lines.
//!
//! The compile pipeline itself (load, check, emit, render) stays in ts-r-check, behind
//! [`build::Host`].

pub mod build;
pub mod buildinfo;
pub mod clock;
pub mod graph;
pub mod incremental;
pub mod references;
pub mod roots;
pub mod status;
