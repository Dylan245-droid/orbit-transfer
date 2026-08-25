pub mod decoder;
pub mod encoder;
pub mod precode;
pub mod simd;
pub mod soliton;

pub use decoder::{DecoderError, FountainDecoder};
pub use encoder::{EncodedSymbol, FountainEncoder};
pub use precode::{hdpc_neighbors, h_for, precode_neighbors, s_for};
pub use simd::{xor_inplace, xor_into};
pub use soliton::SolitonSampler;
