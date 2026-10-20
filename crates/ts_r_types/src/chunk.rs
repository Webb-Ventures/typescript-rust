//! An append-only vector that many threads push to and read from without locks.
//!
//! Elements live in segments that never move, so a `&T` handed out stays valid for the life
//! of the vector. Segment sizes grow geometrically (64, 128, 256, ...), so an empty vector
//! costs a few hundred bytes and a large one wastes at most half a segment: stores are
//! created per program layer and per edit (history, design record 0005), so the fixed cost
//! matters. Indices are reserved with an atomic counter and each slot is a write-once cell:
//! a reader only ever learns an index after its writer published it (the index comes back
//! from `push`, or through a lock-protected intern map), so `get` never sees an empty slot
//! in correct use.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

/// The first segment holds `2^FIRST_BITS` elements; segment `k` holds `2^(FIRST_BITS + k)`.
const FIRST_BITS: u32 = 6;
/// Segments needed for [`MAX_LEN`] elements.
const SEGMENTS: usize = 21;
/// At most 2^26 elements (ids keep their top bits for the store level).
const MAX_LEN: u32 = 1 << 26;

/// One segment: write-once slots.
type Segment<T> = Box<[OnceLock<T>]>;

pub struct ChunkVec<T> {
    segments: [OnceLock<Segment<T>>; SEGMENTS],
    len: AtomicU32,
}

impl<T> std::fmt::Debug for ChunkVec<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkVec")
            .field("len", &self.len())
            .finish()
    }
}

impl<T> Default for ChunkVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Segment and offset of element `i`.
#[inline]
fn locate(i: u32) -> (usize, usize) {
    let j = i as u64 + (1u64 << FIRST_BITS);
    let bit = 63 - j.leading_zeros();
    let seg = (bit - FIRST_BITS) as usize;
    (seg, (j - (1u64 << bit)) as usize)
}

impl<T> ChunkVec<T> {
    pub fn new() -> Self {
        ChunkVec {
            segments: std::array::from_fn(|_| OnceLock::new()),
            len: AtomicU32::new(0),
        }
    }

    /// Number of reserved slots (some may still be in the middle of being written).
    pub fn len(&self) -> usize {
        self.len.load(Ordering::Acquire) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn slot(&self, i: u32) -> &OnceLock<T> {
        let (seg, off) = locate(i);
        let s = self.segments[seg].get_or_init(|| {
            let n = 1usize << (FIRST_BITS as usize + seg);
            let mut v = Vec::with_capacity(n);
            v.resize_with(n, OnceLock::new);
            v.into_boxed_slice()
        });
        &s[off]
    }

    /// Appends `value` and returns its index.
    ///
    /// Panics when the vector is full (2^26 elements): a checker that creates that many types
    /// has already blown its memory budget.
    pub fn push(&self, value: T) -> u32 {
        let i = self.len.fetch_add(1, Ordering::AcqRel);
        assert!(i < MAX_LEN, "type store full");
        let _ = self.slot(i).set(value);
        i
    }

    /// The element at `i`. Panics for an index that was never returned by `push`.
    #[inline]
    pub fn get(&self, i: u32) -> &T {
        match self.try_get(i) {
            Some(v) => v,
            None => panic!("ChunkVec slot {i} read before it was written"),
        }
    }

    /// The element at `i`, or `None` when it is out of range or not yet written.
    #[inline]
    pub fn try_get(&self, i: u32) -> Option<&T> {
        if i >= MAX_LEN {
            return None;
        }
        let (seg, off) = locate(i);
        self.segments[seg].get()?.get(off)?.get()
    }

    /// Approximate heap bytes held (allocated segments).
    pub fn heap_bytes(&self) -> usize {
        self.segments
            .iter()
            .enumerate()
            .filter(|(_, s)| s.get().is_some())
            .map(|(k, _)| {
                (1usize << (FIRST_BITS as usize + k)) * std::mem::size_of::<OnceLock<T>>()
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_get_across_segments() {
        let v = ChunkVec::new();
        for i in 0..20_000u32 {
            assert_eq!(v.push(i * 2), i);
        }
        assert_eq!(*v.get(0), 0);
        assert_eq!(*v.get(63), 126);
        assert_eq!(*v.get(64), 128);
        assert_eq!(*v.get(19_999), 39_998);
        assert!(v.try_get(20_000).is_none());
        assert!(v.try_get(u32::MAX).is_none());
    }

    #[test]
    fn locate_is_contiguous() {
        let mut expect = (0usize, 0usize);
        for i in 0..100_000u32 {
            assert_eq!(locate(i), expect, "{i}");
            expect.1 += 1;
            if expect.1 == 1 << (FIRST_BITS as usize + expect.0) {
                expect = (expect.0 + 1, 0);
            }
        }
        let (seg, _) = locate(MAX_LEN - 1);
        assert!(seg < SEGMENTS);
    }

    #[test]
    fn parallel_pushes() {
        let v = ChunkVec::new();
        std::thread::scope(|s| {
            for t in 0..4u32 {
                let v = &v;
                s.spawn(move || {
                    for i in 0..5000u32 {
                        let id = v.push(t * 100_000 + i);
                        assert_eq!(*v.get(id), t * 100_000 + i);
                    }
                });
            }
        });
        assert_eq!(v.len(), 20_000);
    }
}
