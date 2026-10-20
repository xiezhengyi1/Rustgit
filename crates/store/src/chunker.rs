//! Content-defined chunking ("gear-v1": FastCDC-style normalized chunking).

use std::sync::OnceLock;

use crate::error::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkerConfig {
    pub min: usize,
    pub avg: usize,
    pub max: usize,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        ChunkerConfig {
            min: 64 * 1024,
            avg: 256 * 1024,
            max: 1024 * 1024,
        }
    }
}

impl ChunkerConfig {
    pub fn validate(&self) -> Result<()> {
        if !self.avg.is_power_of_two() || self.avg < 1024 {
            return Err(Error::Unsupported(
                "chunk avg size must be a power of two >= 1024".into(),
            ));
        }
        if !(self.min >= 64 && self.min < self.avg && self.avg < self.max) {
            return Err(Error::Unsupported(
                "chunk sizes must satisfy min < avg < max".into(),
            ));
        }
        Ok(())
    }
}

fn gear() -> &'static [u64; 256] {
    static TABLE: OnceLock<[u64; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut h = blake3::Hasher::new();
        h.update(b"rustgit gear v1");
        let mut xof = h.finalize_xof();
        let mut raw = [0u8; 256 * 8];
        xof.fill(&mut raw);
        let mut t = [0u64; 256];
        for (i, w) in t.iter_mut().enumerate() {
            *w = u64::from_le_bytes(raw[i * 8..i * 8 + 8].try_into().unwrap());
        }
        t
    })
}

fn top_mask(bits: u32) -> u64 {
    ((1u64 << bits) - 1) << (64 - bits)
}

/// Returns the length of the first chunk of `data`.
fn cut(data: &[u8], cfg: &ChunkerConfig, mask_s: u64, mask_l: u64) -> usize {
    let n = data.len();
    if n <= cfg.min {
        return n;
    }
    let limit = n.min(cfg.max);
    let mid = cfg.avg.min(limit);
    let g = gear();
    let mut fp: u64 = 0;
    let mut i = cfg.min;
    while i < mid {
        fp = (fp << 1).wrapping_add(g[data[i] as usize]);
        i += 1;
        if fp & mask_s == 0 {
            return i;
        }
    }
    while i < limit {
        fp = (fp << 1).wrapping_add(g[data[i] as usize]);
        i += 1;
        if fp & mask_l == 0 {
            return i;
        }
    }
    limit
}

/// Length of the first chunk of `data` (for streaming use). `data` should hold at
/// least `cfg.max` bytes unless the input ends.
pub fn first_cut(data: &[u8], cfg: &ChunkerConfig) -> usize {
    let bits = cfg.avg.trailing_zeros();
    cut(data, cfg, top_mask(bits + 2), top_mask(bits - 2))
}

/// Splits `data` into `(offset, len)` ranges. Deterministic for a given config.
pub fn split(data: &[u8], cfg: &ChunkerConfig) -> Vec<(usize, usize)> {
    let bits = cfg.avg.trailing_zeros();
    let mask_s = top_mask(bits + 2);
    let mask_l = top_mask(bits - 2);
    let mut out = Vec::new();
    let mut off = 0;
    while off < data.len() {
        let len = cut(&data[off..], cfg, mask_s, mask_l);
        out.push((off, len));
        off += len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pseudo(seed: u64, n: usize) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn covers_input_and_respects_bounds() {
        let cfg = ChunkerConfig::default();
        let data = pseudo(1, 8 * 1024 * 1024);
        let parts = split(&data, &cfg);
        let mut next = 0;
        for (i, (off, len)) in parts.iter().enumerate() {
            assert_eq!(*off, next);
            assert!(*len <= cfg.max);
            if i + 1 != parts.len() {
                assert!(*len >= cfg.min);
            }
            next += len;
        }
        assert_eq!(next, data.len());
        let avg = data.len() / parts.len();
        assert!(avg > 128 * 1024 && avg < 600 * 1024, "avg chunk {avg}");
    }

    #[test]
    fn insertion_reuses_most_chunks() {
        let cfg = ChunkerConfig::default();
        let a = pseudo(7, 6 * 1024 * 1024);
        let mut b = a.clone();
        b.splice(3_000_000..3_000_000, pseudo(9, 1000));
        let ids = |d: &[u8]| -> std::collections::HashSet<_> {
            split(d, &cfg)
                .into_iter()
                .map(|(o, l)| crate::id::hash_object(crate::id::ObjectType::Chunk, &d[o..o + l]))
                .collect()
        };
        let (ia, ib) = (ids(&a), ids(&b));
        let shared = ia.intersection(&ib).count();
        assert!(shared + 3 >= ia.len(), "shared {shared} of {}", ia.len());
    }

    #[test]
    fn deterministic() {
        let cfg = ChunkerConfig::default();
        let d = pseudo(3, 3_000_000);
        assert_eq!(split(&d, &cfg), split(&d, &cfg));
    }
}
