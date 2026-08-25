use crate::encoder::EncodedSymbol;
use crate::precode;
use crate::simd::xor_inplace;
use crate::soliton::SolitonSampler;
use std::collections::{HashMap, HashSet, VecDeque};
use thiserror::Error;
use xxhash_rust::xxh3::xxh3_64;

#[derive(Error, Debug)]
pub enum DecoderError {
    #[error("Checksum mismatch: expected {expected:#x}, got {got:#x}")]
    ChecksumMismatch { expected: u64, got: u64 },
    #[error("Not enough symbols to decode payload yet ({resolved}/{total_required})")]
    Incomplete { resolved: usize, total_required: usize },
}

/// An active symbol equation in the peeling decoder.
#[derive(Debug, Clone)]
struct ActiveEquation {
    data: Vec<u8>,
    neighbors: HashSet<usize>,
}

/// Fountain Decoder reconstructs source payload from received fountain symbols.
///
/// The decoding graph spans K source symbols plus S LDPC precode checks;
/// reconstruction is complete once all K + S inputs are resolved.
#[derive(Debug)]
pub struct FountainDecoder {
    payload_len: usize,
    symbol_size: usize,
    k: usize,
    l: usize,
    expected_checksum: u64,
    sampler: SolitonSampler,

    resolved_source_symbols: HashMap<usize, Vec<u8>>,
    /// Active equations in slot-based storage (`None` = resolved/removed).
    active_equations: Vec<Option<ActiveEquation>>,
    /// Reverse index: input -> equation slots that still mention it. Lets the
    /// peeling ripple touch only the equations that actually contain the
    /// resolved input, instead of draining every equation each step (the old
    /// O(active_equations) per symbol made the browser decoder the bottleneck).
    rev: HashMap<usize, Vec<usize>>,
    received_esis: HashSet<u32>,
}

impl FountainDecoder {
    /// Creates a new FountainDecoder for an expected payload length, symbol size, and checksum.
    pub fn new(payload_len: usize, symbol_size: usize, k: usize, expected_checksum: u64) -> Self {
        let s = precode::s_for(k);
        let h = precode::h_for(k);
        let l = k + s + h;

        // LDPC constraint equations: for each check i, check_i XOR sources = 0.
        // They participate in peeling from the start, so missing systematic
        // symbols are resolved through the checks instead of stalling the
        // decode until enough random symbols arrive.
        let mut active_equations = Vec::with_capacity(s + h);
        let mut rev: HashMap<usize, Vec<usize>> = HashMap::with_capacity(l * 2);
        let mut add_equation = |data: Vec<u8>, neighbors: HashSet<usize>| {
            let slot = active_equations.len();
            for &n in &neighbors {
                rev.entry(n).or_default().push(slot);
            }
            active_equations.push(Some(ActiveEquation { data, neighbors }));
        };
        for i in 0..s {
            let mut neighbors: HashSet<usize> = precode::precode_neighbors(k, s, i)
                .into_iter()
                .collect();
            neighbors.insert(k + i);
            add_equation(vec![0u8; symbol_size], neighbors);
        }

        // HDPC constraint equations: a dense XOR over the K + S precoded
        // inputs. High degree means they resolve last, acting as the
        // graph's safety net so decoding always completes near K + ε.
        for i in 0..h {
            let mut neighbors: HashSet<usize> = precode::hdpc_neighbors(k, s, h, i)
                .into_iter()
                .collect();
            neighbors.insert(k + s + i);
            add_equation(vec![0u8; symbol_size], neighbors);
        }

        Self {
            payload_len,
            symbol_size,
            k,
            l,
            expected_checksum,
            sampler: SolitonSampler::new(l),
            resolved_source_symbols: HashMap::with_capacity(l),
            active_equations,
            rev,
            received_esis: HashSet::new(),
        }
    }

    /// Returns the number of resolved inputs (sources + precode checks).
    pub fn resolved_count(&self) -> usize {
        self.resolved_source_symbols.len()
    }

    /// Returns the symbol size in bytes.
    pub fn symbol_size(&self) -> usize {
        self.symbol_size
    }

    /// Returns total required source blocks K.
    pub fn k(&self) -> usize {
        self.k
    }

    /// Returns the total number of inputs K + S.
    pub fn l(&self) -> usize {
        self.l
    }

    /// Returns true if all K + S inputs are fully resolved.
    pub fn is_complete(&self) -> bool {
        self.resolved_source_symbols.len() >= self.l
    }

    /// Ingests a received EncodedSymbol and runs peeling resolution.
    /// Returns true if decoding completed on this symbol.
    pub fn add_symbol(&mut self, symbol: EncodedSymbol) -> bool {
        if self.is_complete() || self.received_esis.contains(&symbol.esi) {
            return self.is_complete();
        }

        self.received_esis.insert(symbol.esi);

        let (_degree, neighbors_vec) = self.sampler.get_neighbors(symbol.esi);
        let mut data = symbol.data;
        let mut neighbors = HashSet::with_capacity(neighbors_vec.len());

        // Cancel out already resolved source symbols
        for &n in &neighbors_vec {
            if let Some(resolved_data) = self.resolved_source_symbols.get(&n) {
                xor_inplace(&mut data, resolved_data);
            } else {
                neighbors.insert(n);
            }
        }

        let mut queue = VecDeque::new();

        if neighbors.len() == 1 {
            let single_neighbor = *neighbors.iter().next().unwrap();
            self.resolved_source_symbols.insert(single_neighbor, data);
            queue.push_back(single_neighbor);
        } else if !neighbors.is_empty() {
            let slot = self.active_equations.len();
            for &n in &neighbors {
                self.rev.entry(n).or_default().push(slot);
            }
            self.active_equations.push(Some(ActiveEquation { data, neighbors }));
        }

        // Ripple resolution queue: only equations that actually mention the
        // resolved input are touched (via the reverse index), so each step is
        // O(degree) instead of O(active_equations).
        while let Some(resolved_idx) = queue.pop_front() {
            let resolved_data = self.resolved_source_symbols.get(&resolved_idx).unwrap().clone();

            let eq_slots = self.rev.remove(&resolved_idx).unwrap_or_default();
            for slot in eq_slots {
                let Some(eq) = self.active_equations.get_mut(slot).and_then(|e| e.as_mut()) else {
                    continue; // slot already consumed
                };
                if !eq.neighbors.remove(&resolved_idx) {
                    continue;
                }
                xor_inplace(&mut eq.data, &resolved_data);

                if eq.neighbors.len() == 1 {
                    let new_resolved = *eq.neighbors.iter().next().unwrap();
                    if !self.resolved_source_symbols.contains_key(&new_resolved) {
                        let data = std::mem::take(&mut eq.data);
                        self.active_equations[slot] = None;
                        self.resolved_source_symbols.insert(new_resolved, data);
                        queue.push_back(new_resolved);
                    }
                } else if eq.neighbors.is_empty() {
                    self.active_equations[slot] = None;
                }
            }
        }

        self.is_complete()
    }

    /// Reconstructs full payload buffer and verifies checksum once complete.
    pub fn reconstruct(&self) -> Result<Vec<u8>, DecoderError> {
        if !self.is_complete() {
            return Err(DecoderError::Incomplete {
                resolved: self.resolved_source_symbols.len(),
                total_required: self.l,
            });
        }

        let mut reconstructed = Vec::with_capacity(self.payload_len);
        for i in 0..self.k {
            let block = self.resolved_source_symbols.get(&i).unwrap();
            let remaining = self.payload_len - reconstructed.len();
            let to_copy = remaining.min(self.symbol_size);
            reconstructed.extend_from_slice(&block[..to_copy]);
        }

        let checksum = xxh3_64(&reconstructed);
        if checksum != self.expected_checksum {
            return Err(DecoderError::ChecksumMismatch {
                expected: self.expected_checksum,
                got: checksum,
            });
        }

        Ok(reconstructed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::FountainEncoder;
    use rand::seq::SliceRandom;
    use rand::thread_rng;

    #[test]
    fn test_fountain_end_to_end_lossless() {
        let payload =
            b"Orbit-Transfer rateless fountain code test payload for ultra-fast transfers!".to_vec();
        let symbol_size = 12;
        let encoder = FountainEncoder::new(&payload, symbol_size);
        let l = encoder.l();

        let mut decoder =
            FountainDecoder::new(payload.len(), symbol_size, encoder.k(), encoder.checksum());

        // Feed the full systematic phase (sources + precode checks).
        for esi in 0..l as u32 {
            let sym = encoder.encode_symbol(esi);
            decoder.add_symbol(sym);
        }

        assert!(decoder.is_complete());
        let recovered = decoder.reconstruct().unwrap();
        assert_eq!(recovered, payload);
    }

    #[test]
    fn test_fountain_end_to_end_with_packet_loss() {
        let payload = vec![0x42u8; 10_000]; // 10 KB payload
        let symbol_size = 100;
        let encoder = FountainEncoder::new(&payload, symbol_size);
        let l = encoder.l();

        let mut decoder =
            FountainDecoder::new(payload.len(), symbol_size, encoder.k(), encoder.checksum());

        // Generate a pool of 2L symbols, shuffle, drop 30%.
        let mut symbols: Vec<_> = (0..(l * 2) as u32)
            .map(|esi| encoder.encode_symbol(esi))
            .collect();
        let mut rng = thread_rng();
        symbols.shuffle(&mut rng);

        for sym in symbols {
            if decoder.add_symbol(sym) {
                break;
            }
        }

        assert!(decoder.is_complete());
        let recovered = decoder.reconstruct().unwrap();
        assert_eq!(recovered, payload);
    }

    #[test]
    fn test_precoding_reduces_overhead_under_loss() {
        // With 5% loss, the precoded decoder must complete well below 1.5L
        // symbols (plain LT codes stall around 1.3x at this K).
        let payload = vec![0x7Fu8; 32 * 1024];
        let symbol_size = 128;
        let encoder = FountainEncoder::new(&payload, symbol_size);
        let l = encoder.l();

        let mut decoder =
            FountainDecoder::new(payload.len(), symbol_size, encoder.k(), encoder.checksum());

        let mut received = 0usize;
        let mut esi = 0u32;
        loop {
            if esi % 20 == 19 {
                esi += 1; // simulate 5% loss
                continue;
            }
            let sym = encoder.encode_symbol(esi);
            received += 1;
            if decoder.add_symbol(sym) {
                break;
            }
            esi += 1;
        }

        assert!(decoder.is_complete());
        assert!(
            received <= l + l / 5,
            "precoded decoder should finish within 1.2L symbols, used {received}/{l}"
        );
        assert_eq!(decoder.reconstruct().unwrap(), payload);
    }
}