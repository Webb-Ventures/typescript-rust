//! File stat fingerprints: a machine-local hint that a file still has the content the
//! persisted history hashed, so a warm run can skip reading and hashing it.
//!
//! A fingerprint (length, modification and change times in nanoseconds, inode) is taken
//! *before* the file is read, and kept only when both times are older than
//! [`RACY_SECS`] at that moment: a later write then always moves a time forward, even on
//! file systems with coarse timestamps. Fingerprints live in a sidecar entry next to the
//! manifest, tied to the exact manifest bytes by a digest, so the manifest itself holds no
//! timestamps and stays the same on every machine. A missing or mismatched fingerprint
//! just means the file is read and hashed.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::codec::{Reader, Writer};
use crate::shape::Hash;

/// Times closer than this to "now" are not trusted (writes within one timestamp tick).
pub const RACY_SECS: u64 = 2;
static RACY_NANOS: AtomicU64 = AtomicU64::new(RACY_SECS * 1_000_000_000);

/// Changes the [`RACY_SECS`] window (tests that stamp distinct modification times on every
/// write use 0 to exercise the fingerprint path).
pub fn set_racy_window(d: Duration) {
    RACY_NANOS.store(d.as_nanos().min(u64::MAX as u128) as u64, Ordering::Relaxed);
}
const MAGIC: &[u8; 4] = b"TSRS";

/// What `stat` says about a file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stat {
    pub len: u64,
    pub mtime: u64,
    pub ctime: u64,
    pub ino: u64,
}

fn nanos(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos().min(u64::MAX as u128) as u64)
}

/// The fingerprint of `path`, if it can be trusted now (see the module docs).
pub fn trusted(path: &str) -> Option<Stat> {
    trusted_within(
        path,
        Duration::from_nanos(RACY_NANOS.load(Ordering::Relaxed)),
    )
}

/// [`trusted`] with an explicit window (tests: the global one is shared by every test thread).
fn trusted_within(path: &str, window: Duration) -> Option<Stat> {
    let m = std::fs::metadata(path).ok()?;
    if !m.is_file() {
        return None;
    }
    let s = of(&m);
    let limit = nanos(SystemTime::now().checked_sub(window)?);
    (s.mtime != 0 && s.mtime <= limit && s.ctime <= limit).then_some(s)
}

#[cfg(unix)]
fn of(m: &std::fs::Metadata) -> Stat {
    use std::os::unix::fs::MetadataExt;
    let t = |s: i64, ns: i64| (s.max(0) as u64).saturating_mul(1_000_000_000) + ns.max(0) as u64;
    Stat {
        len: m.len(),
        mtime: t(m.mtime(), m.mtime_nsec()),
        ctime: t(m.ctime(), m.ctime_nsec()),
        ino: m.ino(),
    }
}

#[cfg(not(unix))]
fn of(m: &std::fs::Metadata) -> Stat {
    let mtime = m.modified().map(nanos).unwrap_or(0);
    Stat {
        len: m.len(),
        mtime,
        ctime: m.created().map(nanos).unwrap_or(0),
        ino: 0,
    }
}

/// Fingerprints for one manifest: per lib file and per program file, in manifest order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sidecar {
    pub libs: Vec<Option<Stat>>,
    pub files: Vec<Option<Stat>>,
}

impl Sidecar {
    pub fn encode(&self, manifest: &Hash) -> Vec<u8> {
        let mut w = Writer::default();
        w.out.extend_from_slice(MAGIC);
        w.hash(manifest);
        for list in [&self.libs, &self.files] {
            w.count(list.len());
            for s in list {
                match s {
                    Some(s) => {
                        w.u8(1);
                        for v in [s.len, s.mtime, s.ctime, s.ino] {
                            w.u64(v);
                        }
                    }
                    None => w.u8(0),
                }
            }
        }
        w.out
    }

    /// The fingerprints, if `data` belongs to the manifest with digest `manifest`.
    pub fn decode(data: &[u8], manifest: &Hash) -> Option<Sidecar> {
        let mut r = Reader::new(data);
        let magic = [r.u8()?, r.u8()?, r.u8()?, r.u8()?];
        if &magic != MAGIC || r.hash()? != *manifest {
            return None;
        }
        let mut lists: [Vec<Option<Stat>>; 2] = [Vec::new(), Vec::new()];
        for list in &mut lists {
            for _ in 0..r.count()? {
                list.push(match r.u8()? {
                    0 => None,
                    1 => Some(Stat {
                        len: r.u64()?,
                        mtime: r.u64()?,
                        ctime: r.u64()?,
                        ino: r.u64()?,
                    }),
                    _ => return None,
                });
            }
        }
        let [libs, files] = lists;
        r.done().then_some(Sidecar { libs, files })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_damage() {
        let s = Sidecar {
            libs: vec![None],
            files: vec![
                Some(Stat {
                    len: 1,
                    mtime: 2,
                    ctime: 3,
                    ino: 4,
                }),
                None,
            ],
        };
        let d = [7u8; 32];
        let b = s.encode(&d);
        assert_eq!(Sidecar::decode(&b, &d), Some(s));
        assert_eq!(Sidecar::decode(&b, &[8u8; 32]), None);
        for n in 0..b.len() {
            assert!(Sidecar::decode(&b[..n], &d).is_none());
        }
    }

    #[test]
    fn fresh_files_are_not_trusted() {
        let dir = std::env::temp_dir().join(format!("ts-r-stat-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("a.ts");
        std::fs::write(&p, "x").expect("write");
        let path = p.to_string_lossy().into_owned();
        // The default window: another test in this binary sets the global one to zero.
        let trusted = |p: &str| trusted_within(p, Duration::from_secs(RACY_SECS));
        assert_eq!(trusted(&path), None);
        let old = SystemTime::now() - Duration::from_secs(60);
        let f = std::fs::File::options().write(true).open(&p).expect("open");
        f.set_modified(old).expect("set mtime");
        drop(f);
        // ctime moved with the change above: still not trusted on unix.
        if cfg!(unix) {
            assert_eq!(trusted(&path), None);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
