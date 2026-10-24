//! Dev server core for `ts-r-dev` (milestone M2): unbundled ESM in dev with dependencies
//! served through a pre-bundling seam (decision D3), a module graph with HMR boundary
//! propagation, and a client runtime implementing Vite's `import.meta.hot` API.
//!
//! ```no_run
//! use ts_r_hmr::{DevConfig, DevServer};
//! let server = DevServer::start(DevConfig::new("."))?;
//! println!("listening on {}", server.url());
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! Seams that later milestones fill in:
//! - [`Transform`]: TS/TSX/JSX → JS (today: [`PassthroughJs`] for `.js`/`.mjs`).
//! - [`Resolve`]: module resolution (today: [`SimpleResolver`]; `ts_r_resolver` later).
//! - [`DepBundler`]: `node_modules` pre-bundling (today: [`UnbundledDeps`]; the M3 bundler).
//! - [`PluginHost`]: Vite-style plugin hooks (ts-r-dev: a Node sidecar running the project's
//!   Vite plugins).
//!
//! See `AGENTS.md` in this crate for invariants and the URL space.

pub mod assets;
pub mod cache;
pub mod cors;
pub mod css;
pub mod deps;
pub mod env;
pub mod glob;
pub mod graph;
mod hmr;
pub mod html_proxy;
pub mod http;
pub mod net;
pub mod paths;
pub mod plugin;
pub mod prebundle;
pub mod proxy;
pub mod render;
pub mod resolve;
pub mod scan;
pub mod server;
pub mod sourcemap;
#[doc(hidden)]
pub mod testutil;
pub mod timing;
pub mod transform;
/// The file watcher lives in `ts_r_watch` (shared with `ts-r-check --watch`).
pub use ts_r_watch as watch;
pub mod ws;

pub use graph::{Boundary, FullReload, ModuleGraph, ModuleId, ModuleKind, Propagation};
pub use plugin::{PluginHooks, PluginHost, PluginResolution, ServerHandle, Stage, Transformed};
pub use prebundle::{DepBatch, DepBundler, DepRequest, UnbundledDeps};
pub use resolve::{Resolve, ResolveError, Resolved, SimpleResolver};
pub use scan::{HotInfo, ImportKind, ImportRecord, ModuleScan};
pub use server::{CacheMode, DevConfig, DevServer, Https, Logger};
pub use timing::UpdateTiming;
pub use transform::{
    Mode, PassthroughJs, Transform, TransformError, TransformInput, TransformOutput,
};
pub use watch::{ChangeBatch, ChangeKind, DebounceOptions, FileChange};

// The unit tests build generated graphs (a 200k-module chain).
#[cfg(test)]
ts_r_testing::cap_memory!();
