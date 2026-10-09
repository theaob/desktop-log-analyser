//! Per-chunk Bloom filters over `field=value` pairs (logger, thread, exception, MDC and other
//! structured fields). An exact-match matcher such as `{requestId="req-42"}` skips every chunk
//! whose filter says the pair is absent, without reading the chunk.
//!
//! Hashes are persisted, so they use a fixed algorithm (FNV-1a + splitmix64), never std's
//! randomized hasher.

/// Bits per distinct key; ~1% false positives with 7 hash functions.
const BITS_PER_KEY: usize = 10;
const HASHES: u32 = 7;

fn fnv1a(bytes: &[u8], mut h: u64) -> u64 {
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Hash of a `name=value` pair.
pub fn key_hash(name: &str, value: &str) -> u64 {
    let h = fnv1a(name.as_bytes(), 0xcbf2_9ce4_8422_2325);
    let h = fnv1a(&[0], h);
    fnv1a(value.as_bytes(), h)
}

/// Builds a filter from distinct key hashes.
pub fn build(keys: &[u64]) -> Vec<u8> {
    let bits = (keys.len() * BITS_PER_KEY).max(64).next_multiple_of(8);
    let mut out = vec![0u8; bits / 8];
    for &k in keys {
        for i in probes(k, bits) {
            out[i / 8] |= 1 << (i % 8);
        }
    }
    out
}

fn probes(k: u64, bits: usize) -> impl Iterator<Item = usize> {
    let h1 = k;
    let h2 = splitmix(k) | 1;
    (0..HASHES).map(move |i| (h1.wrapping_add(h2.wrapping_mul(i as u64)) % bits as u64) as usize)
}

/// False means the key is definitely not in the set. An empty filter matches everything.
pub fn may_contain(filter: &[u8], k: u64) -> bool {
    if filter.is_empty() {
        return true;
    }
    let bits = filter.len() * 8;
    probes(k, bits).all(|i| filter[i / 8] & (1 << (i % 8)) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_false_negatives_and_few_false_positives() {
        let keys: Vec<u64> = (0..2000).map(|i| key_hash("requestId", &format!("req-{i}"))).collect();
        let f = build(&keys);
        assert!(keys.iter().all(|k| may_contain(&f, *k)));
        let fp = (5000..15000)
            .filter(|i| may_contain(&f, key_hash("requestId", &format!("req-{i}"))))
            .count();
        assert!(fp < 300, "false positives: {fp}");
        assert_ne!(key_hash("ab", "c"), key_hash("a", "bc"));
    }
}
