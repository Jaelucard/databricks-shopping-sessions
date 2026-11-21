use std::cmp::Ordering;
use std::sync::{Arc, Mutex};

use rustc_hash::FxHashMap;

pub type Key = u64;
const INTERNED: Key = 1 << 63;

const LOW_BITS: u64 = 0x0101010101010101;
const HIGH_BITS: u64 = 0x8080808080808080;

#[inline(always)]
fn compact_eight_ascii(word: u64) -> u64 {
    let pairs = (word & 0x007f_007f_007f_007f) | ((word >> 1) & 0x3f80_3f80_3f80_3f80);
    let quads = (pairs & 0x0000_3fff_0000_3fff) | ((pairs >> 2) & 0x0fff_c000_0fff_c000);
    (quads & 0x0fff_ffff) | ((quads >> 4) & 0x00ff_ffff_f000_0000)
}
#[derive(Default)]
struct Interned {
    names: Vec<Arc<[u8]>>,
    ids: FxHashMap<Arc<[u8]>, Key>,
}

/// Short ASCII keys retain lexical order. Every other UTF-8 string has an exact
/// interned identity in a disjoint namespace. No hash value represents an ID.
#[derive(Default)]
pub struct Keys {
    other: Mutex<Interned>,
}

impl Keys {
    #[inline]
    pub fn key(&self, bytes: &[u8]) -> Key {
        let len = bytes.len();
        if len == 8 || len == 9 {
            let first = u64::from_be_bytes(bytes[..8].try_into().unwrap());
            // Subtracting 1 in each positive ASCII lane cannot borrow.
            // A zero lane introduces a high bit and must use the byte-exact interner.
            let last = if len == 9 { bytes[8] } else { 0 };
            if (first | first.wrapping_sub(LOW_BITS)) & HIGH_BITS == 0
                && (len == 8 || last.wrapping_sub(1) < 127)
            {
                return (compact_eight_ascii(first) << 7) | u64::from(last);
            }
        } else if len < 8 {
            let mut value = 0;
            for &byte in bytes {
                if byte.wrapping_sub(1) >= 127 {
                    return self.intern(bytes);
                }
                value = (value << 7) | u64::from(byte);
            }
            return value << ((9 - len) * 7);
        }
        self.intern(bytes)
    }

    #[cold]
    fn intern(&self, bytes: &[u8]) -> Key {
        let mut other = self.other.lock().unwrap();
        if let Some(&id) = other.ids.get(bytes) {
            return id;
        }
        let key = INTERNED | other.names.len() as Key;
        let name: Arc<[u8]> = bytes.into();
        other.names.push(name.clone());
        other.ids.insert(name, key);
        key
    }

    pub fn name(&self, key: Key) -> String {
        if key & INTERNED != 0 {
            let other = self.other.lock().unwrap();
            return std::str::from_utf8(&other.names[(key ^ INTERNED) as usize])
                .unwrap()
                .to_owned();
        }
        let mut bytes = Vec::with_capacity(9);
        for position in (0..9).rev() {
            let byte = ((key >> (7 * position)) & 127) as u8;
            if byte == 0 {
                break;
            }
            bytes.push(byte);
        }
        String::from_utf8(bytes).unwrap()
    }

    #[inline]
    pub fn cmp(&self, a: Key, b: Key) -> Ordering {
        if (a | b) & INTERNED == 0 || a == b {
            return a.cmp(&b);
        }
        self.name(a).cmp(&self.name(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_and_order_across_namespaces() {
        let k = Keys::default();
        let ids = [
            "",
            "x",
            "x0",
            "0",
            "Z",
            "_",
            "a",
            "é",
            " a",
            "x\0",
            "01234567890",
            "0123456789",
        ];
        for a in ids {
            assert_eq!(k.name(k.key(a.as_bytes())), a);
            for b in ids {
                assert_eq!(k.cmp(k.key(a.as_bytes()), k.key(b.as_bytes())), a.cmp(b));
            }
        }
    }
}

#[cfg(test)]
mod compact_tests {
    use super::*;
    use std::collections::HashMap;
    #[test]
    fn distinct_and_ordered_across_all_ascii_bytes_and_fallbacks() {
        let keys = Keys::default();
        let mut seen = HashMap::new();
        for prefix_len in 0..=10 {
            let mut prev = Vec::new();
            for byte in 0..=127 {
                let mut bytes = vec![b'x'; prefix_len];
                bytes.push(byte);
                let id = keys.key(&bytes);
                assert!(seen.insert(id, bytes.clone()).is_none());
                assert_eq!(keys.name(id).as_bytes(), bytes);
                if !prev.is_empty() {
                    assert_eq!(keys.cmp(keys.key(&prev), id), prev.cmp(&bytes));
                }
                prev = bytes;
            }
        }
        let examples = [
            "",
            "x",
            "x\0",
            "\0",
            "x\0x",
            "12345678\0",
            "1234567\u{7f}x",
            "123456789",
            "1234567890",
            "12345678901",
            "ONE two\r",
            "ＡＢＣ",
            "é",
            "👨🏽‍💻",
        ];
        for a in examples {
            assert_eq!(keys.name(keys.key(a.as_bytes())), a);
            for b in examples {
                assert_eq!(
                    keys.cmp(keys.key(a.as_bytes()), keys.key(b.as_bytes())),
                    a.cmp(b)
                );
                assert_eq!(keys.key(a.as_bytes()) == keys.key(b.as_bytes()), a == b);
            }
        }
    }
}
