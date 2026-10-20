//! Content-addressed, versioned on-disk cache in `~/.cache/ts-r`, shared by every ts-r tool.
//!
//! ```no_run
//! use ts_r_cache::{Cache, KeyBuilder};
//! let cache = Cache::open("lint-results", 1)?;
//! let key = KeyBuilder::new("lint").str(env!("CARGO_PKG_VERSION")).bytes(b"file contents").key();
//! let value = match cache.get(&key) {
//!     Some(blob) => blob.to_vec(),
//!     None => {
//!         let computed = b"[]".to_vec();
//!         cache.put(&key, &computed)?;
//!         computed
//!     }
//! };
//! # let _ = value;
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! See `AGENTS.md` in this crate for the on-disk layout and the invariants.

mod cache;
pub mod dirs;
mod entry;
mod index;
pub mod inspect;
mod key;
mod os;
mod pack;

pub use cache::{
    Cache, DEFAULT_MAX_BYTES, DEFAULT_MMAP_THRESHOLD, DEFAULT_PACK_THRESHOLD, GcReport, Options,
    Stats, TOUCH_GRANULARITY_SECS, Usage,
};
pub use dirs::{bin_dir, cache_root, cache_root_with_source};
pub use entry::{Blob, HEADER_LEN};
pub use index::EntryMeta;
pub use key::{Key, KeyBuilder};
