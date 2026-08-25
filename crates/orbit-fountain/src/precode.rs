/// LDPC + HDPC precoding layers (Raptor-style / RaptorQ-style).
///
/// S LDPC check symbols are computed from the K source symbols and appended
/// to the systematic phase, letting decoding "chain" through them with far
/// fewer fountain symbols than a plain LT code. On top, H HDPC (high-density
/// parity check) symbols are computed from the K + S inputs, giving the
/// decoding graph a dense "safety net" that guarantees completion with
/// near-optimal overhead even at small K (RFC 6330-style structure).
use crate::simd::xor_inplace;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// Number of LDPC precode symbols for K source symbols.
pub fn s_for(k: usize) -> usize {
    if k <= 2 {
        return 2;
    }
    (k / 100 + 10).max(2)
}

/// Number of HDPC (high-density parity check) symbols, chosen as the
/// smallest H such that C(H, floor(H/2)) >= K + S (RFC 6330, §5.3.2).
pub fn h_for(k: usize) -> usize {
    let l = k + s_for(k);
    let mut h = 1;
    while h <= 256 {
        if binomial(h, h / 2) >= l as u128 {
            return h;
        }
        h += 1;
    }
    256
}

/// C(n, k) computed with u128 to avoid overflow up to n = 60.
fn binomial(n: usize, k: usize) -> u128 {
    let k = k.min(n - k);
    let mut c: u128 = 1;
    for i in 0..k {
        c = c * (n - i) as u128 / (i + 1) as u128;
    }
    c
}

/// Source symbol indices covered by LDPC precode symbol `i`.
pub fn precode_neighbors(k: usize, s: usize, i: usize) -> Vec<usize> {
    let mut n = Vec::new();
    let mut j = i;
    while j < k {
        n.push(j);
        j += s;
    }
    n.push(i % k);
    n.push((i + 1) % k);
    n.sort_unstable();
    n.dedup();
    n
}

/// Input indices (over the K + S precoded inputs) covered by HDPC check `i`.
///
/// Dense and deterministic: each check XORs a pseudo-random subset of about
/// half the K + S inputs, seeded from the check index so encoder and decoder
/// agree without any shared dictionary.
pub fn hdpc_neighbors(k: usize, s: usize, _h: usize, i: usize) -> Vec<usize> {
    let l = k + s;
    let mut rng = ChaCha8Rng::seed_from_u64((i as u64) ^ 0x9E3779B97F4A7C15 ^ (l as u64).wrapping_mul(0x85EBCA6B));
    let mut selected = vec![false; l];
    let target = (l as f64 * 0.5) as usize + 1;
    let mut count = 0;
    while count < target {
        let idx = rng.gen_range(0..l);
        if !selected[idx] {
            selected[idx] = true;
            count += 1;
        }
    }
    (0..l).filter(|&j| selected[j]).collect()
}

/// Builds the K + S + H precoded symbol list (source symbols, then LDPC
/// checks, then HDPC checks).
pub fn build_precoded(
    source: &[Vec<u8>],
    s: usize,
    h: usize,
    symbol_size: usize,
) -> Vec<Vec<u8>> {
    let k = source.len();
    let mut precoded: Vec<Vec<u8>> = Vec::with_capacity(k + s + h);
    precoded.extend_from_slice(source);
    for i in 0..s {
        let mut data = vec![0u8; symbol_size];
        for &n in &precode_neighbors(k, s, i) {
            xor_inplace(&mut data, &source[n]);
        }
        precoded.push(data);
    }
    for i in 0..h {
        let mut data = vec![0u8; symbol_size];
        for &n in &hdpc_neighbors(k, s, h, i) {
            xor_inplace(&mut data, &precoded[n]);
        }
        precoded.push(data);
    }
    precoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_count_is_reasonable() {
        assert_eq!(s_for(1), 2);
        assert_eq!(s_for(256), 12);
        assert_eq!(s_for(16384), 173);
    }

    #[test]
    fn hdpc_count_follows_binomial_rule() {
        // C(H, H/2) >= K + S must hold for the chosen H.
        for k in [2, 50, 256, 1000, 4096, 16384] {
            let s = s_for(k);
            let h = h_for(k);
            assert!(
                binomial(h, h / 2) >= (k + s) as u128,
                "k={k}: H={h} must satisfy C(H,H/2) >= K+S={}",
                k + s
            );
        }
    }

    #[test]
    fn neighbors_are_in_range_and_deduped() {
        let k = 100;
        let s = s_for(k);
        for i in 0..s {
            let n = precode_neighbors(k, s, i);
            assert!(n.iter().all(|&x| x < k));
            let mut sorted = n.clone();
            sorted.dedup();
            assert_eq!(n.len(), sorted.len(), "no duplicates");
        }
    }

    #[test]
    fn hdpc_neighbors_dense_and_in_range() {
        let k = 256;
        let s = s_for(k);
        let h = h_for(k);
        for i in 0..h {
            let n = hdpc_neighbors(k, s, h, i);
            assert!(n.iter().all(|&x| x < k + s), "HDPC must reference precoded inputs");
            assert!(
                n.len() >= (k + s) / 4,
                "HDPC check must be dense ({} of {})",
                n.len(),
                k + s
            );
            let mut sorted = n.clone();
            sorted.dedup();
            assert_eq!(n.len(), sorted.len(), "no duplicates");
        }
    }
}