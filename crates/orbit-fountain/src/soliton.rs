use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// Robust Soliton Distribution parameters and deterministic neighbor generator.
#[derive(Debug, Clone)]
pub struct SolitonSampler {
    k: usize,        // Total source symbols
    cdf: Vec<f64>,   // Cumulative Distribution Function table
}

impl SolitonSampler {
    /// Constructs a SolitonSampler for K source symbols.
    pub fn new(k: usize) -> Self {
        let c = 0.1;
        let delta = 0.05;
        let cdf = Self::build_robust_soliton_cdf(k, c, delta);
        Self { k, cdf }
    }

    /// Builds the Robust Soliton cumulative distribution function table.
    fn build_robust_soliton_cdf(k: usize, c: f64, delta: f64) -> Vec<f64> {
        if k <= 1 {
            return vec![1.0];
        }

        let k_f = k as f64;
        let s = c * (k_f / delta).ln() * (k_f.sqrt());
        let pivot = (k_f / s).round() as usize;

        let mut p = vec![0.0; k + 1];

        // Ideal Soliton Distribution
        p[1] = 1.0 / k_f;
        for d in 2..=k {
            p[d] = 1.0 / (d * (d - 1)) as f64;
        }

        // Robust Soliton modifier tau
        let mut tau = vec![0.0; k + 1];
        for d in 1..=k {
            if d < pivot {
                tau[d] = s / (d as f64 * k_f);
            } else if d == pivot {
                tau[d] = (s * (s / delta).ln()) / k_f;
            } else {
                tau[d] = 0.0;
            }
        }

        // Combine Ideal and Robust component Z = sum(p + tau)
        let mut pdf = vec![0.0; k + 1];
        let mut z = 0.0;
        for d in 1..=k {
            pdf[d] = p[d] + tau[d];
            z += pdf[d];
        }

        // Normalize PDF and build CDF
        let mut cdf = vec![0.0; k + 1];
        let mut cumulative = 0.0;
        for d in 1..=k {
            cumulative += pdf[d] / z;
            cdf[d] = cumulative;
        }
        cdf[k] = 1.0; // Ensure max CDF = 1.0
        cdf
    }

    /// Deterministically samples degree and source symbol neighbors for a given Encoding Symbol ID (ESI).
    pub fn get_neighbors(&self, esi: u32) -> (usize, Vec<usize>) {
        if self.k <= 1 {
            return (1, vec![0]);
        }

        // Systematic phase: for ESI < K, return degree 1 directly pointing to source block ESI
        if (esi as usize) < self.k {
            return (1, vec![esi as usize]);
        }

        // Seed PRNG deterministically from ESI
        let mut rng = ChaCha8Rng::seed_from_u64((esi as u64) ^ 0x9E3779B97F4A7C15);

        // Sample degree using CDF inversion
        let u: f64 = rng.gen();
        let mut degree = 1;
        for d in 1..=self.k {
            if u <= self.cdf[d] {
                degree = d;
                break;
            }
        }

        // Sample `degree` unique source symbol indices uniformly without replacement
        let mut neighbors = Vec::with_capacity(degree);
        let mut selected = vec![false; self.k];

        while neighbors.len() < degree {
            let idx = rng.gen_range(0..self.k);
            if !selected[idx] {
                selected[idx] = true;
                neighbors.push(idx);
            }
        }

        neighbors.sort_unstable();
        (degree, neighbors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_soliton_reproducibility() {
        let sampler = SolitonSampler::new(100);
        let (deg1, n1) = sampler.get_neighbors(150);
        let (deg2, n2) = sampler.get_neighbors(150);
        assert_eq!(deg1, deg2);
        assert_eq!(n1, n2);
    }

    #[test]
    fn test_systematic_esi() {
        let sampler = SolitonSampler::new(10);
        for esi in 0..10 {
            let (deg, neighbors) = sampler.get_neighbors(esi);
            assert_eq!(deg, 1);
            assert_eq!(neighbors, vec![esi as usize]);
        }
    }
}
