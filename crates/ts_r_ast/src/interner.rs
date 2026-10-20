//! Per-file string interner: one byte buffer, an `(offset, len)` table and an open-addressed
//! index of `u32` slots. No per-string allocation; everything is plain integers.

use std::fmt;

/// An interned string. Only meaningful together with the [`Interner`] that produced it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Atom(pub u32);

impl Atom {
    /// The empty string, pre-interned in every interner.
    pub const EMPTY: Atom = Atom(0);

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Debug for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Atom({})", self.0)
    }
}

/// Append-only string interner.
#[derive(Clone)]
pub struct Interner {
    buf: String,
    /// `(start, len)` into `buf` per atom.
    entries: Vec<(u32, u32)>,
    /// Open-addressed hash index: 0 = empty, otherwise atom index + 1.
    slots: Vec<u32>,
    /// Hash of each entry, so growing does not re-hash strings.
    hashes: Vec<u32>,
}

impl fmt::Debug for Interner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Interner")
            .field("len", &self.entries.len())
            .finish()
    }
}

impl Default for Interner {
    fn default() -> Self {
        Interner::with_capacity(64)
    }
}

#[inline]
fn hash_bytes(bytes: &[u8]) -> u32 {
    // FxHash-style multiply/rotate over 8-byte chunks; deterministic and fast for short keys.
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut h: u64 = bytes.len() as u64;
    let mut chunks = bytes.chunks_exact(8);
    for c in &mut chunks {
        let v = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
        h = (h.rotate_left(5) ^ v).wrapping_mul(K);
    }
    let rem = chunks.remainder();
    if !rem.is_empty() {
        let mut tail = [0u8; 8];
        tail[..rem.len()].copy_from_slice(rem);
        h = (h.rotate_left(5) ^ u64::from_le_bytes(tail)).wrapping_mul(K);
    }
    (h ^ (h >> 32)) as u32
}

impl Interner {
    pub fn with_capacity(atoms: usize) -> Interner {
        let slots = (atoms * 2).next_power_of_two().max(16);
        let mut i = Interner {
            buf: String::with_capacity(atoms * 8),
            entries: Vec::with_capacity(atoms),
            slots: vec![0; slots],
            hashes: Vec::with_capacity(atoms),
        };
        let empty = i.intern("");
        debug_assert_eq!(empty, Atom::EMPTY);
        i
    }

    /// Number of distinct strings.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the atom for `s`, adding it if new.
    pub fn intern(&mut self, s: &str) -> Atom {
        let h = hash_bytes(s.as_bytes());
        let mask = self.slots.len() - 1;
        let mut pos = h as usize & mask;
        loop {
            let slot = self.slots[pos];
            if slot == 0 {
                break;
            }
            let idx = (slot - 1) as usize;
            if self.hashes[idx] == h && self.get_index(idx) == s {
                return Atom(idx as u32);
            }
            pos = (pos + 1) & mask;
        }
        let idx = self.entries.len() as u32;
        let start = self.buf.len() as u32;
        self.buf.push_str(s);
        self.entries.push((start, s.len() as u32));
        self.hashes.push(h);
        self.slots[pos] = idx + 1;
        if self.entries.len() * 2 > self.slots.len() {
            self.grow();
        }
        Atom(idx)
    }

    /// Looks up `s` without adding it.
    pub fn find(&self, s: &str) -> Option<Atom> {
        let h = hash_bytes(s.as_bytes());
        let mask = self.slots.len() - 1;
        let mut pos = h as usize & mask;
        loop {
            let slot = self.slots[pos];
            if slot == 0 {
                return None;
            }
            let idx = (slot - 1) as usize;
            if self.hashes[idx] == h && self.get_index(idx) == s {
                return Some(Atom(idx as u32));
            }
            pos = (pos + 1) & mask;
        }
    }

    fn grow(&mut self) {
        let new_len = self.slots.len() * 2;
        let mask = new_len - 1;
        let mut slots = vec![0u32; new_len];
        for (idx, &h) in self.hashes.iter().enumerate() {
            let mut pos = h as usize & mask;
            while slots[pos] != 0 {
                pos = (pos + 1) & mask;
            }
            slots[pos] = idx as u32 + 1;
        }
        self.slots = slots;
    }

    fn get_index(&self, idx: usize) -> &str {
        let (start, len) = self.entries[idx];
        &self.buf[start as usize..(start + len) as usize]
    }

    /// The string for `atom`. Atoms from another interner may return garbage or `""`.
    pub fn get(&self, atom: Atom) -> &str {
        match self.entries.get(atom.index()) {
            Some(&(start, len)) => self
                .buf
                .get(start as usize..(start + len) as usize)
                .unwrap_or(""),
            None => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_roundtrip_and_dedup() {
        let mut i = Interner::default();
        assert_eq!(i.get(Atom::EMPTY), "");
        let a = i.intern("foo");
        let b = i.intern("bar");
        assert_ne!(a, b);
        assert_eq!(i.intern("foo"), a);
        assert_eq!(i.get(a), "foo");
        assert_eq!(i.get(b), "bar");
        assert_eq!(i.find("bar"), Some(b));
        assert_eq!(i.find("baz"), None);
        assert_eq!(i.intern(""), Atom::EMPTY);
    }

    #[test]
    fn grows() {
        let mut i = Interner::with_capacity(1);
        let atoms: Vec<Atom> = (0..10_000).map(|n| i.intern(&format!("name{n}"))).collect();
        for (n, a) in atoms.iter().enumerate() {
            assert_eq!(i.get(*a), format!("name{n}"));
            assert_eq!(i.intern(&format!("name{n}")), *a);
        }
        assert_eq!(i.len(), 10_001);
        assert_eq!(i.get(Atom(999_999)), "");
    }
}
