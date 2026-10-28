//! Transform cache: memory first, `ts_r_cache` underneath so a restarted server (or another
//! tool) reuses transforms. Keyed by content hash, never by mtime.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};

use ts_r_cache::{Cache, Key, KeyBuilder};

use crate::scan::{ImportKind, ImportRecord};
use crate::transform::{Mode, TransformOutput};

/// `ts_r_cache` namespace and payload format version. Bump the version when
/// [`encode`]'s layout changes.
pub const NAMESPACE: &str = "dev-transform";
pub const FORMAT_VERSION: u32 = 1;

/// Memory budget before the in-memory layer is cleared (the disk layer keeps everything).
const MEMORY_BUDGET: usize = 256 << 20;

/// Hit/miss counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub memory_hits: u64,
    pub disk_hits: u64,
    pub misses: u64,
}

#[derive(Debug, Default)]
struct Memory {
    map: HashMap<Key, Arc<TransformOutput>>,
    bytes: usize,
    stats: CacheStats,
}

/// A key being computed: requests for it wait for that run instead of running the
/// transform (and plugin hooks with side effects) a second time.
#[derive(Debug, Default)]
struct Flight {
    /// `None` while running; `Some(None)` when the run failed (a waiter runs it itself).
    done: Mutex<Option<Option<Arc<TransformOutput>>>>,
    ready: Condvar,
}

/// Ends a flight however its run ends (an error, or a panic, wakes the waiters too).
struct Landing<'a> {
    cache: &'a TransformCache,
    key: Key,
    flight: Arc<Flight>,
    out: Option<Arc<TransformOutput>>,
}

impl Drop for Landing<'_> {
    fn drop(&mut self) {
        self.cache
            .in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
        *self.flight.done.lock().unwrap_or_else(|e| e.into_inner()) = Some(self.out.take());
        self.flight.ready.notify_all();
    }
}

#[derive(Debug)]
pub struct TransformCache {
    memory: Mutex<Memory>,
    disk: Option<Cache>,
    in_flight: Mutex<HashMap<Key, Arc<Flight>>>,
}

impl TransformCache {
    /// Memory only.
    pub fn in_memory() -> TransformCache {
        TransformCache {
            memory: Mutex::default(),
            disk: None,
            in_flight: Mutex::default(),
        }
    }

    /// Memory plus `ts_r_cache` under `root` (or the default cache root). Falls back to
    /// memory only if the cache directory can't be opened.
    pub fn persistent(root: Option<&Path>) -> TransformCache {
        let disk = match root {
            Some(r) => Cache::open_in(r, NAMESPACE, FORMAT_VERSION),
            None => Cache::open(NAMESPACE, FORMAT_VERSION),
        };
        TransformCache {
            memory: Mutex::default(),
            disk: disk.ok(),
            in_flight: Mutex::default(),
        }
    }

    pub fn is_persistent(&self) -> bool {
        self.disk.is_some()
    }

    /// The cache key: transform identity, mode, module id (root-relative path) and content.
    pub fn key(transform_id: &str, mode: Mode, id: &str, content_hash: &Key) -> Key {
        KeyBuilder::new("ts-r-dev/transform")
            .str(env!("CARGO_PKG_VERSION"))
            .str(transform_id)
            .str(mode.as_str())
            .str(id)
            .key_field(content_hash)
            .key()
    }

    /// Returns the cached output for `key`, or computes, stores and returns it.
    pub fn get_or_insert<E>(
        &self,
        key: Key,
        compute: impl FnOnce() -> Result<TransformOutput, E>,
    ) -> Result<Arc<TransformOutput>, E> {
        self.get_or_insert_rekeyed(key, || compute().map(|out| (out, None)))
    }

    /// [`TransformCache::get_or_insert`] where `compute` may name the key its output belongs
    /// under instead (its inputs turned out to include more than `key` covers): the output
    /// is then stored only under that key.
    pub fn get_or_insert_rekeyed<E>(
        &self,
        key: Key,
        compute: impl FnOnce() -> Result<(TransformOutput, Option<Key>), E>,
    ) -> Result<Arc<TransformOutput>, E> {
        if let Some(hit) = self.lookup(key) {
            return Ok(hit);
        }
        // Concurrent misses of one key (the deps scan warming a module while the browser
        // asks for it) share one run.
        let (flight, leader) = {
            let mut fl = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            match fl.get(&key) {
                Some(f) => (Arc::clone(f), false),
                None => {
                    let f = Arc::new(Flight::default());
                    fl.insert(key, Arc::clone(&f));
                    (f, true)
                }
            }
        };
        if !leader {
            let mut done = flight.done.lock().unwrap_or_else(|e| e.into_inner());
            while done.is_none() {
                done = flight.ready.wait(done).unwrap_or_else(|e| e.into_inner());
            }
            if let Some(Some(out)) = done.as_ref() {
                return Ok(Arc::clone(out));
            }
            drop(done);
            return self.compute_and_store(key, compute);
        }
        let mut landing = Landing {
            cache: self,
            key,
            flight,
            out: None,
        };
        let result = self.compute_and_store(key, compute);
        landing.out = result.as_ref().ok().map(Arc::clone);
        drop(landing);
        result
    }

    fn lookup(&self, key: Key) -> Option<Arc<TransformOutput>> {
        {
            let mut mem = self.memory.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(hit) = mem.map.get(&key).cloned() {
                mem.stats.memory_hits += 1;
                return Some(hit);
            }
        }
        let out = Arc::new(
            self.disk
                .as_ref()?
                .get(&key)
                .and_then(|blob| decode(&blob))?,
        );
        self.remember(key, &out, |s| s.disk_hits += 1);
        Some(out)
    }

    fn compute_and_store<E>(
        &self,
        key: Key,
        compute: impl FnOnce() -> Result<(TransformOutput, Option<Key>), E>,
    ) -> Result<Arc<TransformOutput>, E> {
        let (out, rekey) = compute()?;
        let out = Arc::new(out);
        let key = rekey.unwrap_or(key);
        if let Some(disk) = &self.disk {
            // A failed write only costs a future miss.
            let _ = disk.put(&key, &encode(&out));
        }
        self.remember(key, &out, |s| s.misses += 1);
        Ok(out)
    }

    fn remember(&self, key: Key, out: &Arc<TransformOutput>, count: impl FnOnce(&mut CacheStats)) {
        let mut mem = self.memory.lock().unwrap_or_else(|e| e.into_inner());
        count(&mut mem.stats);
        let size = out.code.len() + out.map.as_ref().map_or(0, String::len) + 64;
        if mem.bytes + size > MEMORY_BUDGET {
            mem.map.clear();
            mem.bytes = 0;
        }
        if mem.map.insert(key, Arc::clone(out)).is_none() {
            mem.bytes += size;
        }
    }

    pub fn stats(&self) -> CacheStats {
        self.memory.lock().unwrap_or_else(|e| e.into_inner()).stats
    }

    /// Makes pending disk entries visible to other processes.
    pub fn flush(&self) {
        if let Some(d) = &self.disk {
            let _ = d.flush();
        }
    }
}

const MAGIC: &[u8; 4] = b"TRDT";

/// Serializes a [`TransformOutput`]: magic, code, optional map, imports.
/// Little-endian `u32` lengths; no absolute paths or timestamps.
pub fn encode(out: &TransformOutput) -> Vec<u8> {
    let mut b = Vec::with_capacity(out.code.len() + 64 + out.imports.len() * 24);
    b.extend_from_slice(MAGIC);
    put_bytes(&mut b, out.code.as_bytes());
    match &out.map {
        Some(m) => {
            b.push(1);
            put_bytes(&mut b, m.as_bytes());
        }
        None => b.push(0),
    }
    b.extend_from_slice(&(out.imports.len() as u32).to_le_bytes());
    for r in &out.imports {
        b.push(r.kind.to_u8());
        b.extend_from_slice(&r.start.to_le_bytes());
        b.extend_from_slice(&r.end.to_le_bytes());
        put_bytes(&mut b, r.specifier.as_bytes());
    }
    b
}

fn put_bytes(b: &mut Vec<u8>, data: &[u8]) {
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(data);
}

/// Inverse of [`encode`]; `None` for anything malformed.
pub fn decode(data: &[u8]) -> Option<TransformOutput> {
    let mut r = Reader { data, pos: 0 };
    if r.take(4)? != MAGIC {
        return None;
    }
    let code = r.string()?;
    let map = match r.take(1)?[0] {
        0 => None,
        1 => Some(r.string()?),
        _ => return None,
    };
    let n = r.u32()? as usize;
    let mut imports = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        let kind = ImportKind::from_u8(r.take(1)?[0])?;
        let start = r.u32()?;
        let end = r.u32()?;
        let specifier = r.string()?;
        if start > end || end as usize > code.len() {
            return None;
        }
        imports.push(ImportRecord {
            specifier,
            start,
            end,
            kind,
        });
    }
    (r.pos == data.len()).then_some(TransformOutput { code, map, imports })
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.data.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::cell::Cell;

    fn sample() -> TransformOutput {
        TransformOutput {
            code: "import a from './a.js';".into(),
            map: Some("{\"version\":3}".into()),
            imports: vec![ImportRecord {
                specifier: "./a.js".into(),
                start: 15,
                end: 21,
                kind: ImportKind::Static,
            }],
        }
    }

    #[test]
    fn encode_round_trips_and_rejects_garbage() {
        let s = sample();
        let bytes = encode(&s);
        assert_eq!(decode(&bytes), Some(s));
        assert_eq!(decode(&bytes[..bytes.len() - 1]), None);
        assert_eq!(decode(b"nope"), None);
        let mut extra = bytes.clone();
        extra.push(0);
        assert_eq!(decode(&extra), None);
    }

    /// Concurrent misses of one key run the transform once; the others get its output.
    #[test]
    fn concurrent_misses_share_one_run() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let cache = TransformCache::in_memory();
        let runs = AtomicU32::new(0);
        let key = TransformCache::key("t/1", Mode::Dev, "src/a.js", &Key::of(b"x"));
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    let out = cache
                        .get_or_insert(key, || -> Result<TransformOutput, ()> {
                            runs.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            Ok(sample())
                        })
                        .unwrap();
                    assert_eq!(*out, sample());
                });
            }
        });
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        // A failed run is not shared: the next caller runs it again.
        let other = TransformCache::key("t/1", Mode::Dev, "src/b.js", &Key::of(b"x"));
        assert!(
            cache
                .get_or_insert(other, || Err::<TransformOutput, _>(()))
                .is_err()
        );
        assert!(cache.get_or_insert(other, || Ok::<_, ()>(sample())).is_ok());
    }

    /// A rekeyed output is stored only under the key its run named.
    #[test]
    fn rekeyed_outputs_are_stored_under_the_new_key() {
        let cache = TransformCache::in_memory();
        let a = TransformCache::key("t/1", Mode::Dev, "src/a.js", &Key::of(b"a"));
        let b = TransformCache::key("t/1", Mode::Dev, "src/a.js", &Key::of(b"b"));
        cache
            .get_or_insert_rekeyed(a, || Ok::<_, ()>((sample(), Some(b))))
            .unwrap();
        let calls = Cell::new(0);
        let count = || -> Result<TransformOutput, ()> {
            calls.set(calls.get() + 1);
            Ok(sample())
        };
        cache.get_or_insert(b, count).unwrap();
        assert_eq!(calls.get(), 0);
        cache.get_or_insert(a, count).unwrap();
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn memory_then_disk_across_instances() {
        let dir = TempDir::new("tcache");
        let calls = Cell::new(0);
        let compute = || -> Result<TransformOutput, ()> {
            calls.set(calls.get() + 1);
            Ok(sample())
        };
        let key = TransformCache::key("t/1", Mode::Dev, "src/a.js", &Key::of(b"source"));
        {
            let c = TransformCache::persistent(Some(dir.path()));
            assert!(c.is_persistent());
            c.get_or_insert(key, compute).unwrap();
            c.get_or_insert(key, compute).unwrap();
            assert_eq!(calls.get(), 1);
            assert_eq!(c.stats().memory_hits, 1);
            c.flush();
        }
        // A "restarted server" finds it on disk.
        let c = TransformCache::persistent(Some(dir.path()));
        let out = c.get_or_insert(key, compute).unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(*out, sample());
        assert_eq!(c.stats().disk_hits, 1);
        // Different content or transform id → different key.
        assert_ne!(
            key,
            TransformCache::key("t/2", Mode::Dev, "src/a.js", &Key::of(b"source"))
        );
        assert_ne!(
            key,
            TransformCache::key("t/1", Mode::Dev, "src/a.js", &Key::of(b"source2"))
        );
    }
}
