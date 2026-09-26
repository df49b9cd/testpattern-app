pub mod json;

/// Stable 64-bit FNV-1a (cache keys and synthetic ids must not change
/// between builds, unlike std's DefaultHasher).
pub fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
