//! The [`Cache`] handle: one namespace (tool or data kind) at one format version.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::entry::{self, Blob, HEADER_LEN};
use crate::index::{
    self, EntryMeta, JournalRecord, KIND_PUT, KIND_REMOVE, KIND_TOUCH, LOOSE, Overlay, Rec,
    Snapshot,
};
use crate::os;
use crate::pack::{self, SegmentWriter};
use crate::{Key, dirs};

/// Entries (header + payload) smaller than this go into pack segments; bigger ones get their
/// own memory-mapped file.
pub const DEFAULT_PACK_THRESHOLD: u64 = 1024 * 1024;
/// Loose entries at least this big are memory-mapped instead of copied.
pub const DEFAULT_MMAP_THRESHOLD: u64 = 64 * 1024;
/// Default size budget per namespace for [`Cache::maybe_gc`].
pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// A read only refreshes an entry's `last_used` if the recorded value is older than this, so a
/// warm run that reads everything writes nothing.
pub const TOUCH_GRANULARITY_SECS: u64 = 60 * 60;
/// Temp files older than this are assumed abandoned (crashed writer) and removed by `gc`.
const TMP_MAX_AGE: Duration = Duration::from_secs(60 * 60);
/// On a miss, re-read the index at most this often to pick up other processes' writes.
const REFRESH_INTERVAL: Duration = Duration::from_millis(50);
/// Segments smaller than this are merged by `gc` once there are [`MERGE_MIN_SEGMENTS`] of them.
const SMALL_SEGMENT: u64 = 4 * 1024 * 1024;
const MERGE_MIN_SEGMENTS: usize = 4;
/// Directory names a namespace may not use.
const RESERVED: &[&str] = &["bin", "tmp"];

/// Tuning knobs. `Options::default()` is right for tools; tests shrink the thresholds.
#[derive(Clone, Debug)]
pub struct Options {
    /// Entries whose size (header + payload) is below this are packed.
    pub pack_threshold: u64,
    /// Loose entries at least this big are memory-mapped on read.
    pub mmap_threshold: u64,
    /// Fold the journal into the index snapshot once it reaches this many bytes.
    pub compact_journal_bytes: u64,
    /// Current time in seconds since the Unix epoch, used for LRU bookkeeping. Tests inject a
    /// fake clock here.
    pub clock: fn() -> u64,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            pack_threshold: DEFAULT_PACK_THRESHOLD,
            mmap_threshold: DEFAULT_MMAP_THRESHOLD,
            compact_journal_bytes: 4096 * index::JOURNAL_RECORD as u64,
            clock: now_secs,
        }
    }
}

/// Counters for this `Cache` handle (this process only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Reads that returned a verified value.
    pub hits: u64,
    /// Reads that found nothing (including corrupt entries).
    pub misses: u64,
    /// Entries found corrupt and dropped.
    pub corrupt: u64,
    /// Successful writes.
    pub puts: u64,
    /// Payload bytes returned by hits.
    pub bytes_read: u64,
    /// Payload bytes written.
    pub bytes_written: u64,
    /// Unexpected I/O errors swallowed by reads (reported as misses). The first one is
    /// described by [`Cache::first_io_error`].
    pub io_errors: u64,
}

#[derive(Debug, Default)]
struct Counters {
    hits: AtomicU64,
    misses: AtomicU64,
    corrupt: AtomicU64,
    puts: AtomicU64,
    bytes_read: AtomicU64,
    bytes_written: AtomicU64,
    io_errors: AtomicU64,
}

fn bump(c: &AtomicU64, by: u64) {
    c.fetch_add(by, Ordering::Relaxed);
}

/// Entry count and bytes for one namespace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Number of live entries.
    pub entries: u64,
    /// Total size of live entries, headers included (excludes dead space in pack segments).
    pub bytes: u64,
}

/// What a [`Cache::gc`] run did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Another process was already collecting or compacting this namespace; nothing was done.
    pub skipped_busy: bool,
    /// Live entries found before eviction.
    pub before: Usage,
    /// Live entries left afterwards.
    pub after: Usage,
    /// Entries evicted to get under budget.
    pub evicted: u64,
    /// Pack segments rewritten to reclaim dead space or merge small segments.
    pub segments_rewritten: u64,
    /// Files deleted: emptied segments, abandoned temp files, files with invalid names.
    pub files_removed: u64,
}

/// A content-addressed store for one namespace at one format version, rooted at
/// `<root>/<namespace>/v<format_version>/`.
///
/// Reads never lock. Small entries are appended to a pack segment owned by this handle and
/// become visible to other processes on [`Cache::flush`] (also run on drop); large entries are
/// written to a temp file and renamed into place. Many handles (threads or processes) may use
/// the same directory at once.
#[derive(Debug)]
pub struct Cache {
    dir: PathBuf,
    objects: PathBuf,
    packs: PathBuf,
    tmp: PathBuf,
    namespace: String,
    format_version: u32,
    options: Options,
    counters: Counters,
    /// What the first error counted in `io_errors` was, for diagnostics.
    first_io_error: OnceLock<String>,
    /// Index records not yet appended to the journal.
    pending: Mutex<Vec<JournalRecord>>,
    /// What this handle changed since `view` was loaded (puts, removes, touches).
    local: RwLock<Overlay>,
    /// Snapshot + journal, loaded on first use and refreshed on misses.
    view: RwLock<Option<Arc<View>>>,
    last_refresh: Mutex<Option<Instant>>,
    /// The segment this handle appends small entries to.
    writer: Mutex<Option<SegmentWriter>>,
    /// Id of the segment in `writer` (0 = none), so reads can skip the writer lock.
    writer_id: AtomicU64,
    /// Flushed segments mapped by this handle.
    segments: RwLock<HashMap<u64, Arc<memmap2::Mmap>>>,
}

struct View {
    snapshot: Snapshot,
    overlay: Overlay,
    stamp: Stamp,
}

impl std::fmt::Debug for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("View")
            .field("snapshot", &self.snapshot.len())
            .field("overlay", &self.overlay.len())
            .finish()
    }
}

impl View {
    fn get(&self, key: &Key) -> Option<Rec> {
        match self.overlay.get(key) {
            Some(v) => *v,
            None => self.snapshot.get(key),
        }
    }
}

/// Identity of the index + journal files, to tell whether a view is stale.
type Stamp = [Option<(u64, Option<SystemTime>)>; 2];

impl Cache {
    /// Opens `namespace` at `format_version` under the default root ([`dirs::cache_root`]).
    pub fn open(namespace: &str, format_version: u32) -> io::Result<Cache> {
        Cache::open_in(&dirs::cache_root(), namespace, format_version)
    }

    /// Opens a namespace under an explicit root.
    pub fn open_in(root: &Path, namespace: &str, format_version: u32) -> io::Result<Cache> {
        Cache::with_options(root, namespace, format_version, Options::default())
    }

    /// Opens a namespace with custom [`Options`].
    pub fn with_options(
        root: &Path,
        namespace: &str,
        format_version: u32,
        options: Options,
    ) -> io::Result<Cache> {
        validate_namespace(namespace)?;
        let dir = root.join(namespace).join(format!("v{format_version}"));
        let objects = dir.join("objects");
        let packs = dir.join("packs");
        let tmp = dir.join("tmp");
        for d in [&objects, &packs, &tmp] {
            fs::create_dir_all(d)?;
        }
        let options = Options {
            mmap_threshold: options.mmap_threshold.max(HEADER_LEN as u64 + 1),
            ..options
        };
        Ok(Cache {
            dir,
            objects,
            packs,
            tmp,
            namespace: namespace.to_owned(),
            format_version,
            options,
            counters: Counters::default(),
            first_io_error: OnceLock::new(),
            pending: Mutex::new(Vec::new()),
            local: RwLock::new(HashMap::new()),
            view: RwLock::new(None),
            last_refresh: Mutex::new(None),
            writer: Mutex::new(None),
            writer_id: AtomicU64::new(LOOSE),
            segments: RwLock::new(HashMap::new()),
        })
    }

    /// The namespace directory (`<root>/<namespace>/v<format_version>`).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The namespace name.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The format version this handle reads and writes.
    pub fn format_version(&self) -> u32 {
        self.format_version
    }

    fn now(&self) -> u64 {
        (self.options.clock)()
    }

    fn object_path(&self, key: &Key) -> PathBuf {
        let hex = key.to_hex();
        self.objects.join(&hex[..2]).join(&hex[2..])
    }

    // ---- index view ------------------------------------------------------------------------

    fn stamp(&self) -> Stamp {
        let st = |name: &str| {
            fs::metadata(self.dir.join(name))
                .ok()
                .map(|m| (m.len(), m.modified().ok()))
        };
        [st("index"), st("journal")]
    }

    fn load_view(&self) -> View {
        let stamp = self.stamp();
        let snapshot = Snapshot::load(&self.dir.join("index"));
        let mut overlay = Overlay::new();
        let (recs, _) = index::read_journal(&self.dir.join("journal"));
        index::fold_journal(&recs, &mut overlay, |k| snapshot.get(k));
        View {
            snapshot,
            overlay,
            stamp,
        }
    }

    fn view(&self) -> Arc<View> {
        if let Some(v) = read_lock(&self.view).as_ref() {
            return v.clone();
        }
        let mut slot = write_lock(&self.view);
        slot.get_or_insert_with(|| Arc::new(self.load_view()))
            .clone()
    }

    /// Reloads the view if the index or journal changed. `force` skips the rate limit.
    /// Returns whether a new view was loaded.
    fn refresh(&self, force: bool) -> bool {
        {
            let mut last = lock(&self.last_refresh);
            let now = Instant::now();
            if !force && last.is_some_and(|t| now.duration_since(t) < REFRESH_INTERVAL) {
                return false;
            }
            *last = Some(now);
        }
        if self.view().stamp == self.stamp() {
            return false;
        }
        let fresh = Arc::new(self.load_view());
        *write_lock(&self.view) = Some(fresh);
        true
    }

    fn lookup(&self, key: &Key) -> Option<Rec> {
        if let Some(v) = read_lock(&self.local).get(key) {
            return *v;
        }
        self.view().get(key)
    }

    fn record(&self, key: Key, kind: u8, rec: Rec) {
        lock(&self.pending).push(JournalRecord { key, kind, rec });
        write_lock(&self.local).insert(key, (kind != KIND_REMOVE).then_some(rec));
    }

    // ---- reads -----------------------------------------------------------------------------

    /// Reads and verifies an entry. Returns `None` on a miss. A corrupt entry is dropped and
    /// reported as a miss; I/O errors are reported as misses too. Never panics, never takes a
    /// file lock.
    pub fn get(&self, key: &Key) -> Option<Blob> {
        let found = match self.lookup(key) {
            Some(rec) if rec.seg != LOOSE => self.get_packed(key, rec),
            Some(_) => self.get_loose(key),
            None => match self.get_loose(key) {
                Some(found) => Some(found),
                // Another process may have packed it since we loaded the index.
                None if self.refresh(false) => match self.view().get(key) {
                    Some(rec) if rec.seg != LOOSE => self.get_packed(key, rec),
                    _ => None,
                },
                None => None,
            },
        };
        let Some((blob, rec)) = found else {
            bump(&self.counters.misses, 1);
            return None;
        };
        bump(&self.counters.hits, 1);
        bump(&self.counters.bytes_read, blob.len() as u64);
        let now = self.now();
        let stale = match self.lookup(key) {
            Some(r) if r.same_location(&rec) => {
                r.last_used.saturating_add(TOUCH_GRANULARITY_SECS) < now
            }
            _ => true,
        };
        if stale {
            self.record(
                *key,
                KIND_TOUCH,
                Rec {
                    last_used: now,
                    ..rec
                },
            );
        }
        Some(blob)
    }

    /// Counts an I/O error swallowed by a read and remembers the first one.
    fn io_error(&self, op: &str, path: &Path, e: &io::Error) {
        bump(&self.counters.io_errors, 1);
        self.first_io_error.get_or_init(|| {
            let path = path.strip_prefix(&self.dir).unwrap_or(path);
            format!("{op} {}: {e} ({:?})", path.display(), e.kind())
        });
    }

    fn get_loose(&self, key: &Key) -> Option<(Blob, Rec)> {
        let path = self.object_path(key);
        let file = match os::retry_busy(|| File::open(&path)) {
            Ok(f) => f,
            Err(e) => {
                // Missing, or (Windows) evicted by a gc while another process had it mapped.
                if !os::is_gone(&e) {
                    self.io_error("open loose entry", &path, &e);
                }
                return None;
            }
        };
        let blob = match self.read_loose(&file) {
            Ok(b) => b,
            Err(_) => {
                // A short read means the file is not what its metadata said: corrupt.
                self.discard_loose(&path, file, None);
                return None;
            }
        };
        if entry::verify(blob.raw(), key, self.format_version).is_err() {
            self.discard_loose(&path, file, Some(blob));
            return None;
        }
        let rec = Rec {
            size: blob.raw().len() as u64,
            last_used: 0,
            seg: LOOSE,
            offset: 0,
        };
        Some((blob, rec))
    }

    fn read_loose(&self, file: &File) -> io::Result<Blob> {
        let len = file.metadata()?.len();
        if len >= self.options.mmap_threshold {
            // SAFETY: loose entry files are immutable once renamed into place. Writers always
            // create a fresh temp file and rename it; gc and corruption handling only unlink,
            // which leaves existing mappings valid. Truncation by a process outside ts-r could
            // raise SIGBUS; AGENTS.md documents that as outside the cache's contract.
            #[allow(unsafe_code)]
            let map = unsafe { memmap2::Mmap::map(file)? };
            return Ok(Blob::mapped(map));
        }
        let len = usize::try_from(len).map_err(|_| io::Error::other("entry too large"))?;
        let mut buf = vec![0u8; len];
        (&*file).read_exact(&mut buf)?;
        Ok(Blob::heap(buf))
    }

    /// Deletes a corrupt loose entry, unless another process already replaced it.
    fn discard_loose(&self, path: &Path, file: File, blob: Option<Blob>) {
        bump(&self.counters.corrupt, 1);
        let same_file = same_file(&file, path);
        // Unmap and close before deleting (required on Windows).
        drop(blob);
        drop(file);
        if same_file {
            let _ = fs::remove_file(path);
        }
    }

    fn get_packed(&self, key: &Key, rec: Rec) -> Option<(Blob, Rec)> {
        match self.read_packed(key, rec) {
            Ok(Some(blob)) => Some((blob, rec)),
            Ok(None) => {
                // Corrupt: drop it from the index; gc reclaims the space.
                bump(&self.counters.corrupt, 1);
                self.record(*key, KIND_REMOVE, rec);
                None
            }
            // On Windows a deleted segment another process still maps is "gone" too.
            Err(e) if os::is_gone(&e) => {
                // gc rewrote or deleted the segment since we loaded the index.
                if self.refresh(true)
                    && let Some(new) = self
                        .view()
                        .get(key)
                        .filter(|n| n.seg != LOOSE && !n.same_location(&rec))
                {
                    return self.read_packed(key, new).ok().flatten().map(|b| (b, new));
                }
                None
            }
            Err(e) => {
                let path = pack::segment_path(&self.packs, rec.seg);
                self.io_error("read packed entry", &path, &e);
                None
            }
        }
    }

    /// `Ok(None)` = present but corrupt.
    fn read_packed(&self, key: &Key, rec: Rec) -> io::Result<Option<Blob>> {
        let end = rec.offset.saturating_add(rec.size);
        let (Ok(start), Ok(end)) = (usize::try_from(rec.offset), usize::try_from(end)) else {
            return Ok(None);
        };
        // Entries in the segment this handle is still writing are read back through the
        // writer; everything else comes from a shared map of a flushed segment.
        let own = if self.writer_id.load(Ordering::Acquire) == rec.seg {
            let mut writer = lock(&self.writer);
            match writer.as_mut() {
                Some(w) if w.id == rec.seg => Some(Blob::heap(w.read_at(rec.offset, rec.size)?)),
                _ => None,
            }
        } else {
            None
        };
        let blob = match own {
            Some(b) => b,
            None => {
                let map = self.segment(rec.seg)?;
                if end > map.len() {
                    return Ok(None);
                }
                Blob::packed(map, start, end)
            }
        };
        Ok(entry::verify(blob.raw(), key, self.format_version)
            .is_ok()
            .then_some(blob))
    }

    fn segment(&self, id: u64) -> io::Result<Arc<memmap2::Mmap>> {
        if let Some(m) = read_lock(&self.segments).get(&id) {
            return Ok(m.clone());
        }
        let map = pack::map_segment(&pack::segment_path(&self.packs, id))?;
        write_lock(&self.segments).insert(id, map.clone());
        Ok(map)
    }

    /// Whether an entry exists, without reading it. A hint: an entry evicted by another
    /// process after this handle loaded its index can still be reported; [`Cache::get`] is
    /// authoritative.
    pub fn contains(&self, key: &Key) -> bool {
        self.lookup(key).is_some() || self.object_path(key).is_file()
    }

    /// Index metadata for an entry, if this handle knows about it.
    pub fn meta(&self, key: &Key) -> Option<EntryMeta> {
        self.lookup(key).map(|r| r.meta())
    }

    // ---- writes ----------------------------------------------------------------------------

    /// Stores a value, replacing any previous value for `key`. Readers never see a partial
    /// entry: small entries become visible to other processes on [`Cache::flush`], large ones
    /// when the rename into place completes.
    pub fn put(&self, key: &Key, payload: &[u8]) -> io::Result<()> {
        let header = entry::encode_header(key, self.format_version, payload);
        let size = (HEADER_LEN + payload.len()) as u64;
        if size < self.options.pack_threshold {
            self.put_packed(key, &header, payload, size)?;
        } else {
            self.put_loose(key, &header, payload)?;
            let rec = Rec {
                size,
                last_used: self.now(),
                seg: LOOSE,
                offset: 0,
            };
            self.record(*key, KIND_PUT, rec);
        }
        bump(&self.counters.puts, 1);
        bump(&self.counters.bytes_written, payload.len() as u64);
        Ok(())
    }

    /// Appends to this handle's segment and queues the PUT record before releasing the writer
    /// lock: a flush (or gc) that takes the lock next must publish the record with the segment,
    /// or gc would find the segment unreferenced and delete it.
    fn put_packed(&self, key: &Key, header: &[u8], payload: &[u8], size: u64) -> io::Result<()> {
        let mut writer = lock(&self.writer);
        if writer.as_ref().is_some_and(|w| w.len >= pack::ROTATE_BYTES) {
            self.flush_locked(&mut writer)?;
        }
        let w = match writer.as_mut() {
            Some(w) => w,
            None => {
                let w = writer.insert(SegmentWriter::create(&self.packs)?);
                self.writer_id.store(w.id, Ordering::Release);
                w
            }
        };
        match w.append(header, payload) {
            Ok(offset) => {
                let rec = Rec {
                    size,
                    last_used: self.now(),
                    seg: w.id,
                    offset,
                };
                self.record(*key, KIND_PUT, rec);
                Ok(())
            }
            Err(e) => {
                // The segment may now hold a partial entry; start a fresh one next time. The
                // partial bytes are never referenced, so readers cannot see them.
                let _ = self.flush_locked(&mut writer);
                Err(e)
            }
        }
    }

    fn put_loose(&self, key: &Key, header: &[u8], payload: &[u8]) -> io::Result<()> {
        let tmp = self.write_temp(header, payload)?;
        let dest = self.object_path(key);
        let rename = || os::retry_busy(|| fs::rename(&tmp, &dest));
        let result = match rename() {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                rename()
            }
            other => other,
        };
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
            // On Windows a rename onto an open (mapped) file can fail. If the key already has
            // a value, that is as good as ours. If another process is deleting the old file
            // (delete pending), the key is being evicted: dropping our value is the same as
            // storing it and having it evicted, and the next read is a miss.
            if !dest.is_file() && !os::is_delete_pending(&e) {
                return Err(e);
            }
        }
        Ok(())
    }

    fn write_temp(&self, header: &[u8], payload: &[u8]) -> io::Result<PathBuf> {
        let path = self.tmp.join(temp_name());
        let mut f = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // Someone deleted the cache directory while we were running: recreate it.
                fs::create_dir_all(&self.tmp)?;
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?
            }
            Err(e) => return Err(e),
        };
        let written = f.write_all(header).and_then(|()| f.write_all(payload));
        if let Err(e) = written {
            drop(f);
            let _ = fs::remove_file(&path);
            return Err(e);
        }
        Ok(path)
    }

    /// Deletes an entry. Returns whether it existed.
    pub fn remove(&self, key: &Key) -> io::Result<bool> {
        let rec = self.lookup(key);
        let loose = match fs::remove_file(self.object_path(key)) {
            Ok(()) => true,
            Err(e) if e.kind() == io::ErrorKind::NotFound => false,
            Err(e) => return Err(e),
        };
        let existed = loose || rec.is_some_and(|r| r.seg != LOOSE);
        if existed {
            let rec = rec.unwrap_or(Rec {
                size: 0,
                last_used: 0,
                seg: LOOSE,
                offset: 0,
            });
            self.record(*key, KIND_REMOVE, rec);
        }
        Ok(existed)
    }

    /// This handle's counters.
    pub fn stats(&self) -> Stats {
        let c = &self.counters;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        Stats {
            hits: get(&c.hits),
            misses: get(&c.misses),
            corrupt: get(&c.corrupt),
            puts: get(&c.puts),
            bytes_read: get(&c.bytes_read),
            bytes_written: get(&c.bytes_written),
            io_errors: get(&c.io_errors),
        }
    }

    /// The first I/O error counted in [`Stats::io_errors`], if any: the operation, the file
    /// (relative to [`Cache::dir`]) and the OS error. For diagnostics (`ts-r doctor`, tests).
    pub fn first_io_error(&self) -> Option<&str> {
        self.first_io_error.get().map(String::as_str)
    }

    /// Publishes this handle's writes to other processes: writes out the pack segment, appends
    /// pending index records to the journal (one write), then closes the segment. Compacts
    /// the journal if it has grown large. Called on drop.
    pub fn flush(&self) -> io::Result<()> {
        let journal_len = {
            let mut writer = lock(&self.writer);
            self.flush_locked(&mut writer)?
        };
        if journal_len >= self.options.compact_journal_bytes {
            self.compact()?;
        }
        Ok(())
    }

    /// Requires the writer lock. Returns the journal size after appending.
    fn flush_locked(&self, writer: &mut MutexGuard<'_, Option<SegmentWriter>>) -> io::Result<u64> {
        let mut recs = std::mem::take(&mut *lock(&self.pending));
        let mut finished = Ok(());
        if let Some(w) = writer.as_mut()
            && let Err(e) = w.finish()
        {
            // Its entries cannot be published: drop their records so nobody looks for them.
            // gc later recovers or deletes the abandoned `.open` file.
            let id = w.id;
            recs.retain(|r| r.rec.seg != id);
            write_lock(&self.local).retain(|_, v| v.is_none_or(|r| r.seg != id));
            finished = Err(e);
        }
        let appended = if recs.is_empty() {
            Ok(0)
        } else {
            FileLock::shared(&self.dir.join("index.lock"))
                .and_then(|_shared| index::append_journal(&self.dir.join("journal"), &recs))
        };
        // Only now, with its records in the journal, is the segment released (unlocked).
        // Readers may map it from here on; it is never written again.
        **writer = None;
        self.writer_id.store(LOOSE, Ordering::Release);
        finished?;
        appended
    }

    /// Folds the journal into a new index snapshot. Returns `false` if another process is
    /// already doing maintenance (compaction or gc) on this namespace.
    pub fn compact(&self) -> io::Result<bool> {
        let Some(_maint) = FileLock::try_exclusive(&self.dir.join("gc.lock"))? else {
            return Ok(false);
        };
        let _index = FileLock::exclusive(&self.dir.join("index.lock"))?;
        let snapshot = Snapshot::load(&self.dir.join("index"));
        let (recs, _) = index::read_journal(&self.dir.join("journal"));
        let mut overlay = Overlay::new();
        index::fold_journal(&recs, &mut overlay, |k| snapshot.get(k));
        let entries = index::merged(&snapshot, &overlay);
        self.write_snapshot_locked(&entries)?;
        Ok(true)
    }

    /// Requires `index.lock` held exclusively. Writes the snapshot, then empties the journal.
    fn write_snapshot_locked(&self, entries: &[(Key, Rec)]) -> io::Result<()> {
        let bytes = index::encode_snapshot(entries);
        fs::create_dir_all(&self.tmp)?;
        let tmp = self.tmp.join(temp_name());
        fs::write(&tmp, &bytes)?;
        if let Err(e) = os::retry_busy(|| fs::rename(&tmp, self.dir.join("index"))) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        match OpenOptions::new()
            .write(true)
            .open(self.dir.join("journal"))
        {
            Ok(f) => f.set_len(0),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Live entries and bytes according to the index (fast; does not scan files). Loose
    /// entries written by a process that crashed before flushing are missing until `gc`.
    pub fn usage(&self) -> Usage {
        let view = self.load_view();
        let mut overlay = view.overlay;
        let pending = lock(&self.pending).clone();
        index::fold_journal(&pending, &mut overlay, |k| view.snapshot.get(k));
        usage_of(&index::merged(&view.snapshot, &overlay))
    }

    // ---- gc --------------------------------------------------------------------------------

    /// Runs [`Cache::gc`] down to 80% of `max_bytes` if [`Cache::usage`] exceeds `max_bytes`.
    pub fn maybe_gc(&self, max_bytes: u64) -> io::Result<Option<GcReport>> {
        if self.usage().bytes <= max_bytes {
            return Ok(None);
        }
        self.gc(max_bytes - max_bytes / 5).map(Some)
    }

    /// Evicts least-recently-used entries until the namespace holds at most `max_bytes`,
    /// rewrites pack segments that are mostly dead (or merges small ones), removes abandoned
    /// files and rewrites the index from a scan of the files.
    ///
    /// Only one process does maintenance on a namespace at a time (`gc.lock`); a concurrent
    /// call returns immediately with `skipped_busy`. Readers are never blocked, and writers
    /// only for the two short moments the index is read and replaced.
    pub fn gc(&self, max_bytes: u64) -> io::Result<GcReport> {
        let Some(_maint) = FileLock::try_exclusive(&self.dir.join("gc.lock"))? else {
            return Ok(GcReport {
                skipped_busy: true,
                ..GcReport::default()
            });
        };
        // Publish our own writes first so the scan sees them.
        {
            let mut writer = lock(&self.writer);
            self.flush_locked(&mut writer)?;
        }

        // Phase 1: read the index (journal appends wait only for this moment).
        let (state, journal_mark, index_damaged) = {
            let _index = FileLock::exclusive(&self.dir.join("index.lock"))?;
            let index_path = self.dir.join("index");
            let snapshot = Snapshot::load(&index_path);
            let (recs, len) = index::read_journal(&self.dir.join("journal"));
            // A damaged index or journal may have lost track of flushed segments.
            let damaged = (snapshot.len() == 0
                && fs::metadata(&index_path).is_ok_and(|m| m.len() > 0))
                || recs.len() as u64 * index::JOURNAL_RECORD as u64 != len;
            let mut overlay = Overlay::new();
            index::fold_journal(&recs, &mut overlay, |k| snapshot.get(k));
            let state: BTreeMap<Key, Rec> =
                index::merged(&snapshot, &overlay).into_iter().collect();
            (state, len, damaged)
        };

        // Phase 2: scan, evict and rewrite without holding the index lock.
        let mut report = GcReport::default();
        let mut live: Vec<(Key, Rec)> = Vec::with_capacity(state.len());
        self.scan_loose(&state, &mut live, &mut report);
        let segments = self.scan_segments();
        for (key, rec) in &state {
            let fits = rec.seg != LOOSE
                && segments
                    .get(&rec.seg)
                    .is_some_and(|s| rec.offset.saturating_add(rec.size) <= s.len);
            if fits {
                live.push((*key, *rec));
            }
        }
        let referenced: BTreeSet<u64> = state.values().map(|r| r.seg).collect();
        let mut doomed =
            self.recover_unreferenced(&segments, &referenced, &state, index_damaged, &mut live);
        report.before = usage_of(&live);

        // Oldest first; key order breaks ties so the result is deterministic.
        live.sort_unstable_by(|a, b| a.1.last_used.cmp(&b.1.last_used).then(a.0.cmp(&b.0)));
        let mut total = report.before.bytes;
        let mut kept: Vec<(Key, Rec)> = Vec::with_capacity(live.len());
        for (key, rec) in live {
            if total > max_bytes {
                let gone = rec.seg != LOOSE
                    || match fs::remove_file(self.object_path(&key)) {
                        Ok(()) => true,
                        Err(e) => e.kind() == io::ErrorKind::NotFound,
                    };
                if gone {
                    total -= rec.size;
                    report.evicted += 1;
                    continue;
                }
                // Still in use (Windows) or unremovable: keep it indexed.
            }
            kept.push((key, rec));
        }

        let rewriter =
            self.rewrite_segments(&mut kept, &segments, &referenced, &mut doomed, &mut report)?;
        kept.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        // Phase 3: apply what other processes appended meanwhile, then replace the index.
        {
            let _index = FileLock::exclusive(&self.dir.join("index.lock"))?;
            let journal = fs::read(self.dir.join("journal")).unwrap_or_default();
            let mark = usize::try_from(journal_mark)
                .unwrap_or(usize::MAX)
                .min(journal.len());
            let newer = index::decode_journal(&journal[mark..]);
            let base: HashMap<Key, Rec> = kept.iter().copied().collect();
            let mut overlay = Overlay::new();
            index::fold_journal(&newer, &mut overlay, |k| base.get(k).copied());
            let mut final_entries = index::merged_with(&kept, &overlay);
            // A writer that flushed after phase 1 may have put entries into a segment that was
            // recovered and then emptied above; those records must not point at a deleted file.
            final_entries.retain(|(_, r)| !doomed.contains(&r.seg));
            report.after = usage_of(&final_entries);
            self.write_snapshot_locked(&final_entries)?;
        }
        // The new index no longer references the doomed segments; release the new one.
        drop(rewriter);
        for id in doomed {
            let pack = fs::remove_file(pack::segment_path(&self.packs, id)).is_ok();
            let open = fs::remove_file(pack::open_segment_path(&self.packs, id)).is_ok();
            report.files_removed += u64::from(pack || open);
        }
        write_lock(&self.segments).clear();
        write_lock(&self.local).clear();
        *write_lock(&self.view) = None;
        report.files_removed += self.remove_old_temp_files();
        report.files_removed += self.remove_stale_lock_files();
        Ok(report)
    }

    /// Collects loose entries, deleting files that are not valid entry names or that the index
    /// says were superseded by a packed write.
    fn scan_loose(
        &self,
        state: &BTreeMap<Key, Rec>,
        live: &mut Vec<(Key, Rec)>,
        report: &mut GcReport,
    ) {
        for shard in read_dir_or_empty(&self.objects) {
            let shard_name = shard.file_name();
            for file in read_dir_or_empty(&shard.path()) {
                let name = file.file_name();
                let key = match (shard_name.to_str(), name.to_str()) {
                    (Some(a), Some(b)) if a.len() == 2 => Key::from_hex(&format!("{a}{b}")),
                    _ => None,
                };
                let Ok(md) = file.metadata() else { continue };
                let indexed = key.and_then(|k| state.get(&k));
                let superseded = indexed.is_some_and(|r| r.seg != LOOSE);
                let Some(key) = key.filter(|_| md.is_file() && !superseded) else {
                    if fs::remove_file(file.path()).is_ok() {
                        report.files_removed += 1;
                    }
                    continue;
                };
                // The index knows about reads; the mtime only covers entries whose writer
                // crashed before flushing its index records.
                let last_used = match indexed {
                    Some(r) => r.last_used,
                    None => md.modified().ok().map(secs_since_epoch).unwrap_or(0),
                };
                live.push((
                    key,
                    Rec {
                        size: md.len(),
                        last_used,
                        seg: LOOSE,
                        offset: 0,
                    },
                ));
            }
        }
    }

    fn scan_segments(&self) -> BTreeMap<u64, SegmentInfo> {
        let mut out = BTreeMap::new();
        for e in read_dir_or_empty(&self.packs) {
            let Some((id, open)) = e.file_name().to_str().and_then(pack::parse_segment_name) else {
                continue;
            };
            let Ok(md) = e.metadata() else { continue };
            out.insert(
                id,
                SegmentInfo {
                    len: md.len(),
                    modified: md.modified().ok(),
                    open,
                },
            );
        }
        out
    }

    /// Handles segments the index does not reference (see `pack.rs`). A locked one is still
    /// being written and is left alone. An unlocked `.open` one was left by a writer that
    /// crashed: its intact entries are recovered into `live` and it is renamed to `.pack`. An
    /// unreferenced `.pack` holds only removed or evicted entries, unless the index was
    /// damaged, in which case it is recovered too. Returns the segments to delete.
    fn recover_unreferenced(
        &self,
        segments: &BTreeMap<u64, SegmentInfo>,
        referenced: &BTreeSet<u64>,
        state: &BTreeMap<Key, Rec>,
        index_damaged: bool,
        live: &mut Vec<(Key, Rec)>,
    ) -> BTreeSet<u64> {
        let now = SystemTime::now();
        let mut doomed = BTreeSet::new();
        // Each key is recovered at most once (a snapshot with a duplicate key is invalid): not
        // if it is indexed or already live (a loose file), nor from a second segment.
        let mut taken: BTreeSet<Key> = live.iter().map(|(k, _)| *k).collect();
        for (&id, info) in segments {
            if referenced.contains(&id) {
                continue;
            }
            let path = info.path(&self.packs, id);
            let young = info
                .modified
                .and_then(|m| now.duration_since(m).ok())
                .is_none_or(|age| age < pack::ORPHAN_AGE);
            // An empty `.open` file may belong to a writer that has not locked it yet.
            if (info.open && info.len == 0 && young) || pack::is_locked(&self.packs, id) {
                continue;
            }
            if !info.open && !index_damaged {
                doomed.insert(id);
                continue;
            }
            let last_used = info.modified.map(secs_since_epoch).unwrap_or(0);
            let found = pack::map_segment(&path)
                .map(|map| pack::scan_entries(&map, self.format_version))
                .unwrap_or_default();
            let mut seen = BTreeSet::new();
            let recovered: Vec<_> = found
                .into_iter()
                // Newest first, so a key put twice keeps its later value.
                .rev()
                .filter(|(key, _, _)| {
                    !state.contains_key(key) && !taken.contains(key) && seen.insert(*key)
                })
                .map(|(key, offset, size)| {
                    (
                        key,
                        Rec {
                            size,
                            last_used,
                            seg: id,
                            offset,
                        },
                    )
                })
                .collect();
            let published =
                !info.open || fs::rename(&path, pack::segment_path(&self.packs, id)).is_ok();
            if recovered.is_empty() || !published {
                doomed.insert(id);
            } else {
                taken.extend(seen);
                live.extend(recovered);
            }
        }
        doomed
    }

    /// Rewrites segments that are mostly dead, or merges small ones, copying their live
    /// entries into a new segment (updating `kept`). Adds every segment that no longer holds a
    /// live entry to `doomed`, to delete once the new index is in place. Returns the writer
    /// holding the new segment, to release after that.
    fn rewrite_segments(
        &self,
        kept: &mut Vec<(Key, Rec)>,
        segments: &BTreeMap<u64, SegmentInfo>,
        referenced: &BTreeSet<u64>,
        doomed: &mut BTreeSet<u64>,
        report: &mut GcReport,
    ) -> io::Result<Option<SegmentWriter>> {
        let mut live_bytes: BTreeMap<u64, u64> = BTreeMap::new();
        for (_, rec) in kept.iter().filter(|(_, r)| r.seg != LOOSE) {
            *live_bytes.entry(rec.seg).or_default() += rec.size;
        }
        let small: BTreeSet<u64> = segments
            .iter()
            .filter(|(id, s)| s.len < SMALL_SEGMENT && live_bytes.contains_key(id))
            .map(|(id, _)| *id)
            .collect();
        let merge_small = small.len() >= MERGE_MIN_SEGMENTS;
        let mut rewrite = BTreeSet::new();
        for (&id, info) in segments {
            match live_bytes.get(&id) {
                Some(&live) => {
                    if live.saturating_mul(2) < info.len || (merge_small && small.contains(&id)) {
                        rewrite.insert(id);
                    }
                }
                // Every entry was evicted. Only a segment the index referenced is known to be
                // flushed; an unreferenced one may have been skipped as in use.
                None if referenced.contains(&id) => {
                    doomed.insert(id);
                }
                None => {}
            }
        }
        if rewrite.is_empty() {
            return Ok(None);
        }
        let mut writer = SegmentWriter::create(&self.packs)?;
        let mut order: Vec<usize> = (0..kept.len())
            .filter(|&i| rewrite.contains(&kept[i].1.seg))
            .collect();
        order.sort_unstable_by_key(|&i| (kept[i].1.seg, kept[i].1.offset));
        let mut dropped = BTreeSet::new();
        for i in order {
            let (key, rec) = kept[i];
            match self.read_packed(&key, rec) {
                Ok(Some(blob)) => {
                    let raw = blob.raw();
                    let offset = writer.append(&raw[..HEADER_LEN], &raw[HEADER_LEN..])?;
                    kept[i].1 = Rec {
                        seg: writer.id,
                        offset,
                        ..rec
                    };
                }
                // Corrupt or unreadable: drop it.
                _ => {
                    dropped.insert(i);
                }
            }
        }
        // Renamed to `.pack` now; still locked until the caller drops it after phase 3.
        writer.finish()?;
        if !dropped.is_empty() {
            let mut i = 0;
            kept.retain(|_| {
                let keep = !dropped.contains(&i);
                i += 1;
                keep
            });
        }
        report.segments_rewritten = rewrite.len() as u64;
        doomed.extend(rewrite);
        Ok(Some(writer))
    }

    /// Deletes segment lock files no writer holds, once their segment is published (`.pack`:
    /// its writer is done, or gc recovered it after a crash) or gone. A lock file without a
    /// segment must also be older than `ORPHAN_AGE`: a writer creates it just before the
    /// segment.
    fn remove_stale_lock_files(&self) -> u64 {
        let now = SystemTime::now();
        let mut removed = 0;
        for e in read_dir_or_empty(&self.packs) {
            let Some(id) = e.file_name().to_str().and_then(pack::parse_lock_name) else {
                continue;
            };
            if pack::open_segment_path(&self.packs, id).exists() {
                continue;
            }
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age > pack::ORPHAN_AGE);
            if !old && !pack::segment_path(&self.packs, id).exists() {
                continue;
            }
            if pack::lock_state(&e.path()) == pack::LockState::Free
                && fs::remove_file(e.path()).is_ok()
            {
                removed += 1;
            }
        }
        removed
    }

    fn remove_old_temp_files(&self) -> u64 {
        let now = SystemTime::now();
        let mut removed = 0;
        for t in read_dir_or_empty(&self.tmp) {
            let old = t
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age > TMP_MAX_AGE);
            if old && fs::remove_file(t.path()).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[derive(Clone, Copy, Debug)]
struct SegmentInfo {
    len: u64,
    modified: Option<SystemTime>,
    /// Still named `.open` (being written, or its writer crashed).
    open: bool,
}

impl SegmentInfo {
    fn path(&self, packs: &Path, id: u64) -> PathBuf {
        if self.open {
            pack::open_segment_path(packs, id)
        } else {
            pack::segment_path(packs, id)
        }
    }
}

fn usage_of(entries: &[(Key, Rec)]) -> Usage {
    Usage {
        entries: entries.len() as u64,
        bytes: entries.iter().map(|(_, r)| r.size).sum(),
    }
}

fn validate_namespace(ns: &str) -> io::Result<()> {
    let ok = !ns.is_empty()
        && ns.len() <= 64
        && ns
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && !RESERVED.contains(&ns);
    if ok {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid cache namespace {ns:?}: use 1-64 of [a-z0-9_-], not bin/tmp"),
        ))
    }
}

// A panic while holding these locks cannot leave the data inconsistent (plain pushes, inserts
// and replacements), so poisoning is ignored.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn read_lock<T>(m: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    m.read().unwrap_or_else(|p| p.into_inner())
}

fn write_lock<T>(m: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    m.write().unwrap_or_else(|p| p.into_inner())
}

/// An advisory lock on a lock file, released when dropped (the file is closed).
struct FileLock {
    _file: File,
}

impl FileLock {
    fn open(path: &Path) -> io::Result<File> {
        let open = || {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)
        };
        match open() {
            // The cache directory was deleted while we were running: recreate it.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                open()
            }
            other => other,
        }
    }

    fn exclusive(path: &Path) -> io::Result<FileLock> {
        let f = FileLock::open(path)?;
        f.lock()?;
        Ok(FileLock { _file: f })
    }

    fn shared(path: &Path) -> io::Result<FileLock> {
        let f = FileLock::open(path)?;
        f.lock_shared()?;
        Ok(FileLock { _file: f })
    }

    fn try_exclusive(path: &Path) -> io::Result<Option<FileLock>> {
        let f = FileLock::open(path)?;
        match f.try_lock() {
            Ok(()) => Ok(Some(FileLock { _file: f })),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(e)) => Err(e),
        }
    }
}

fn read_dir_or_empty(path: &Path) -> impl Iterator<Item = fs::DirEntry> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
}

fn temp_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{}-{n}-{nanos}.tmp", std::process::id())
}

fn secs_since_epoch(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_secs() -> u64 {
    secs_since_epoch(SystemTime::now())
}

#[cfg(unix)]
fn same_file(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), fs::metadata(path)) {
        (Ok(a), Ok(b)) => a.ino() == b.ino() && a.dev() == b.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(_file: &File, path: &Path) -> bool {
    path.is_file()
}
