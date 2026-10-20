//! Cache keys: 32-byte BLAKE3 hashes of everything that determines a value.

use std::fmt;

/// A cache key. Build one with [`KeyBuilder`] from every input that affects the cached value
/// (tool version, compiler options, file content, ...), or with [`Key::of`] for a single blob.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key([u8; 32]);

impl Key {
    /// Key length in bytes.
    pub const LEN: usize = 32;

    /// Wraps raw hash bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Key {
        Key(bytes)
    }

    /// The BLAKE3 hash of `data`.
    pub fn of(data: &[u8]) -> Key {
        Key(*blake3::hash(data).as_bytes())
    }

    /// The raw bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex, 64 characters.
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push(HEX[(b >> 4) as usize] as char);
            s.push(HEX[(b & 15) as usize] as char);
        }
        s
    }

    /// Parses 64 lowercase or uppercase hex characters.
    pub fn from_hex(s: &str) -> Option<Key> {
        let s = s.as_bytes();
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, pair) in s.chunks_exact(2).enumerate() {
            out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Some(Key(out))
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Key({})", self.to_hex())
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Builds a [`Key`] from several inputs without ambiguity: every field is length-prefixed, so
/// `("ab", "c")` and `("a", "bc")` hash differently.
///
/// ```
/// use ts_r_cache::KeyBuilder;
/// let key = KeyBuilder::new("ts-r-build/ast")
///     .str("0.1.0")           // tool version
///     .bytes(b"{\"target\":\"es2022\"}") // normalised compiler options
///     .bytes(b"export const x = 1;")    // file content
///     .key();
/// assert_eq!(key.to_hex().len(), 64);
/// ```
#[derive(Clone, Debug)]
pub struct KeyBuilder(blake3::Hasher);

impl KeyBuilder {
    /// Starts a key. `domain` separates kinds of data (for example `"ts-r-lint/results"`).
    pub fn new(domain: &str) -> KeyBuilder {
        let mut b = KeyBuilder(blake3::Hasher::new());
        b.str(domain);
        b
    }

    /// Adds a byte field.
    pub fn bytes(&mut self, data: &[u8]) -> &mut KeyBuilder {
        self.0.update(&(data.len() as u64).to_le_bytes());
        self.0.update(data);
        self
    }

    /// Adds a string field.
    pub fn str(&mut self, s: &str) -> &mut KeyBuilder {
        self.bytes(s.as_bytes())
    }

    /// Adds an integer field.
    pub fn u64(&mut self, v: u64) -> &mut KeyBuilder {
        self.bytes(&v.to_le_bytes())
    }

    /// Adds another key (for example the key of a dependency).
    pub fn key_field(&mut self, k: &Key) -> &mut KeyBuilder {
        self.bytes(k.as_bytes())
    }

    /// Finishes the key. The builder can keep being used.
    pub fn key(&self) -> Key {
        Key(*self.0.finalize().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let k = Key::of(b"hello");
        let hex = k.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(Key::from_hex(&hex), Some(k));
        assert_eq!(Key::from_hex(&hex.to_uppercase()), Some(k));
        assert_eq!(Key::from_hex("zz"), None);
        assert_eq!(Key::from_hex(&"g".repeat(64)), None);
    }

    #[test]
    fn builder_is_unambiguous() {
        let a = KeyBuilder::new("d").str("ab").str("c").key();
        let b = KeyBuilder::new("d").str("a").str("bc").key();
        assert_ne!(a, b);
        let c = KeyBuilder::new("e").str("ab").str("c").key();
        assert_ne!(a, c);
        assert_eq!(a, KeyBuilder::new("d").str("ab").str("c").key());
    }
}
