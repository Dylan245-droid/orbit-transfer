use crate::precode;
use crate::simd::{xor_inplace, xor_into};
use crate::soliton::SolitonSampler;
use xxhash_rust::xxh3::xxh3_64;

/// Encoded symbol structure sent over the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedSymbol {
    pub esi: u32,
    pub symbol_size: usize,
    pub data: Vec<u8>,
}

/// Fountain Encoder partitions payload data into K source symbols, adds S
/// LDPC precode checks, and generates rateless fountain symbols over the
/// K + S inputs.
#[derive(Debug, Clone)]
pub struct FountainEncoder {
    payload_len: usize,
    symbol_size: usize,
    k: usize,
    l: usize,
    precoded: Vec<Vec<u8>>,
    sampler: SolitonSampler,
    checksum: u64,
}

impl FountainEncoder {
    /// Creates a FountainEncoder from raw payload data and target symbol size.
    pub fn new(payload: &[u8], symbol_size: usize) -> Self {
        assert!(symbol_size > 0, "Symbol size must be > 0");
        let payload_len = payload.len();
        let checksum = xxh3_64(payload);

        let k = (payload_len + symbol_size - 1) / symbol_size;
        let k = if k == 0 { 1 } else { k };

        let mut source_symbols = Vec::with_capacity(k);
        for i in 0..k {
            let start = i * symbol_size;
            let end = (start + symbol_size).min(payload_len);
            let mut symbol_data = vec![0u8; symbol_size];
            if start < payload_len {
                symbol_data[..end - start].copy_from_slice(&payload[start..end]);
            }
            source_symbols.push(symbol_data);
        }

        let s = precode::s_for(k);
        let h = precode::h_for(k);
        let l = k + s + h;
        let precoded = precode::build_precoded(&source_symbols, s, h, symbol_size);
        let sampler = SolitonSampler::new(l);

        Self {
            payload_len,
            symbol_size,
            k,
            l,
            precoded,
            sampler,
            checksum,
        }
    }

    /// Returns the number of source blocks K.
    pub fn k(&self) -> usize {
        self.k
    }

    /// Returns the number of encodable inputs K + S (source + precode).
    pub fn l(&self) -> usize {
        self.l
    }

    /// Returns total original payload length in bytes.
    pub fn payload_len(&self) -> usize {
        self.payload_len
    }

    /// Returns symbol size in bytes.
    pub fn symbol_size(&self) -> usize {
        self.symbol_size
    }

    /// Returns original XXH3 64-bit checksum of full payload.
    pub fn checksum(&self) -> u64 {
        self.checksum
    }

    /// Encodes a single symbol for a given Encoding Symbol ID (ESI).
    pub fn encode_symbol(&self, esi: u32) -> EncodedSymbol {
        let (_degree, neighbors) = self.sampler.get_neighbors(esi);

        let mut data = vec![0u8; self.symbol_size];

        if neighbors.len() == 1 {
            data.copy_from_slice(&self.precoded[neighbors[0]]);
        } else if neighbors.len() == 2 {
            xor_into(
                &mut data,
                &self.precoded[neighbors[0]],
                &self.precoded[neighbors[1]],
            );
        } else {
            xor_into(
                &mut data,
                &self.precoded[neighbors[0]],
                &self.precoded[neighbors[1]],
            );
            for &idx in &neighbors[2..] {
                xor_inplace(&mut data, &self.precoded[idx]);
            }
        }

        EncodedSymbol {
            esi,
            symbol_size: self.symbol_size,
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fountain_encoder_basic() {
        let payload = b"Hello, High-Performance World! Orbit-Transfer Fountain Engine.";
        let encoder = FountainEncoder::new(payload, 16);
        assert!(encoder.k() > 0);
        assert!(encoder.l() >= encoder.k());

        let sym0 = encoder.encode_symbol(0);
        assert_eq!(sym0.esi, 0);
        assert_eq!(sym0.symbol_size, 16);
    }
}