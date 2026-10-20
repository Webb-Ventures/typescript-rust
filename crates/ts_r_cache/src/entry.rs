//! On-disk entry format and the [`Blob`] handed back by reads.
//!
//! Every entry file is a 128-byte header followed by the payload:
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0 | 4 | magic `TSRC` |
//! | 4 | 4 | entry layout version (this table), little-endian `u32` |
//! | 8 | 4 | namespace format version, `u32` |
//! | 12 | 4 | reserved, zero |
//! | 16 | 8 | payload length, `u64` |
//! | 24 | 32 | key |
//! | 56 | 32 | BLAKE3 of the payload |
//! | 88 | 40 | zero padding |
//!
//! The same format is used for loose entry files and for entries inside pack segments (where
//! each entry starts at a multiple of 128). The header is 128 bytes, so a memory-mapped payload
//! always starts 128-byte aligned, which lets callers read index-based data (`u32` arrays) in
//! place.

use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use crate::Key;

pub(crate) const MAGIC: [u8; 4] = *b"TSRC";
pub(crate) const ENTRY_LAYOUT: u32 = 1;
/// Size of the entry header; the payload starts at this offset.
pub const HEADER_LEN: usize = 128;

pub(crate) fn encode_header(key: &Key, format_version: u32, payload: &[u8]) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[0..4].copy_from_slice(&MAGIC);
    h[4..8].copy_from_slice(&ENTRY_LAYOUT.to_le_bytes());
    h[8..12].copy_from_slice(&format_version.to_le_bytes());
    h[16..24].copy_from_slice(&(payload.len() as u64).to_le_bytes());
    h[24..56].copy_from_slice(key.as_bytes());
    h[56..88].copy_from_slice(blake3::hash(payload).as_bytes());
    h
}

/// Why an entry was rejected. Any of these means "delete it and treat as a miss".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Corrupt {
    TooShort,
    Magic,
    Layout,
    FormatVersion,
    Length,
    Key,
    Checksum,
}

/// Checks the whole file (header + payload) and returns `Ok` only if it is intact.
pub(crate) fn verify(file: &[u8], key: &Key, format_version: u32) -> Result<(), Corrupt> {
    if file.len() < HEADER_LEN {
        return Err(Corrupt::TooShort);
    }
    let (h, payload) = file.split_at(HEADER_LEN);
    if h[0..4] != MAGIC {
        return Err(Corrupt::Magic);
    }
    if u32_at(h, 4) != ENTRY_LAYOUT {
        return Err(Corrupt::Layout);
    }
    if u32_at(h, 8) != format_version {
        return Err(Corrupt::FormatVersion);
    }
    if u64_at(h, 16) != payload.len() as u64 {
        return Err(Corrupt::Length);
    }
    if h[24..56] != key.as_bytes()[..] {
        return Err(Corrupt::Key);
    }
    if h[56..88] != blake3::hash(payload).as_bytes()[..] {
        return Err(Corrupt::Checksum);
    }
    Ok(())
}

pub(crate) fn u32_at(b: &[u8], at: usize) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[at..at + 4]);
    u32::from_le_bytes(a)
}

pub(crate) fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

/// A verified cache value. Dereferences to the payload bytes.
///
/// Large entries are memory-mapped (no copy; the OS can drop the pages under pressure), small
/// ones are read into a heap buffer. Either way the checksum was verified before it was returned.
pub struct Blob {
    repr: Repr,
}

enum Repr {
    Heap(Vec<u8>),
    Mapped(memmap2::Mmap),
    /// A range of a shared, memory-mapped pack segment.
    Packed {
        map: Arc<memmap2::Mmap>,
        start: usize,
        end: usize,
    },
}

impl Blob {
    pub(crate) fn heap(buf: Vec<u8>) -> Blob {
        Blob {
            repr: Repr::Heap(buf),
        }
    }

    pub(crate) fn mapped(map: memmap2::Mmap) -> Blob {
        Blob {
            repr: Repr::Mapped(map),
        }
    }

    /// `start..end` must be in bounds (the caller checks against the map length).
    pub(crate) fn packed(map: Arc<memmap2::Mmap>, start: usize, end: usize) -> Blob {
        Blob {
            repr: Repr::Packed { map, start, end },
        }
    }

    /// The whole entry including the header, as read from disk.
    pub(crate) fn raw(&self) -> &[u8] {
        match &self.repr {
            Repr::Heap(v) => v,
            Repr::Mapped(m) => m,
            Repr::Packed { map, start, end } => &map[*start..*end],
        }
    }

    /// Whether the payload is served from a memory map (no copy) rather than a heap buffer.
    pub fn is_mapped(&self) -> bool {
        !matches!(self.repr, Repr::Heap(_))
    }

    /// Copies the payload into a `Vec`.
    pub fn to_vec(&self) -> Vec<u8> {
        self.deref().to_vec()
    }
}

impl Deref for Blob {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.raw()[HEADER_LEN..]
    }
}

impl AsRef<[u8]> for Blob {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl fmt::Debug for Blob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Blob")
            .field("len", &self.len())
            .field("mapped", &self.is_mapped())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(key: &Key, v: u32, payload: &[u8]) -> Vec<u8> {
        let mut f = encode_header(key, v, payload).to_vec();
        f.extend_from_slice(payload);
        f
    }

    #[test]
    fn round_trip_and_rejections() {
        let k = Key::of(b"k");
        let f = file(&k, 3, b"payload");
        assert_eq!(verify(&f, &k, 3), Ok(()));
        assert_eq!(verify(&f, &k, 4), Err(Corrupt::FormatVersion));
        assert_eq!(verify(&f, &Key::of(b"other"), 3), Err(Corrupt::Key));
        assert_eq!(verify(&f[..f.len() - 1], &k, 3), Err(Corrupt::Length));
        assert_eq!(verify(&f[..10], &k, 3), Err(Corrupt::TooShort));
        let mut bad = f.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(verify(&bad, &k, 3), Err(Corrupt::Checksum));
        let mut bad = f.clone();
        bad[0] = b'X';
        assert_eq!(verify(&bad, &k, 3), Err(Corrupt::Magic));
        let mut bad = f;
        bad[4] = 9;
        assert_eq!(verify(&bad, &k, 3), Err(Corrupt::Layout));
    }

    #[test]
    fn empty_payload_is_valid() {
        let k = Key::of(b"");
        assert_eq!(verify(&file(&k, 1, b""), &k, 1), Ok(()));
    }
}
