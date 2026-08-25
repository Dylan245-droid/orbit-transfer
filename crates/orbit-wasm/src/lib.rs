//! WASM bindings: fountain encode/decode in the browser, demo-ready.
use orbit_fountain::{FountainDecoder, FountainEncoder};
use wasm_bindgen::prelude::*;

/// Encodes a payload into `count` fountain symbols (Uint8Arrays).
/// Returns [esi, data, ...] flattened for JS convenience.
#[wasm_bindgen]
pub struct Fountain {
    encoder: FountainEncoder,
}

#[wasm_bindgen]
impl Fountain {
    #[wasm_bindgen(constructor)]
    pub fn new(payload: &[u8], symbol_size: usize) -> Fountain {
        Self {
            encoder: FountainEncoder::new(payload, symbol_size),
        }
    }

    #[wasm_bindgen(js_name = k)]
    pub fn k(&self) -> u32 {
        self.encoder.k() as u32
    }

    #[wasm_bindgen(js_name = l)]
    pub fn l(&self) -> u32 {
        self.encoder.l() as u32
    }

    #[wasm_bindgen(js_name = payloadLen)]
    pub fn payload_len(&self) -> usize {
        self.encoder.payload_len()
    }

    #[wasm_bindgen(js_name = symbolSize)]
    pub fn symbol_size(&self) -> usize {
        self.encoder.symbol_size()
    }

    /// XXH3 checksum as hex string (u64 cannot cross wasm-bindgen cleanly).
    #[wasm_bindgen(js_name = checksumHex)]
    pub fn checksum_hex(&self) -> String {
        format!("{:016x}", self.encoder.checksum())
    }

    #[wasm_bindgen(js_name = encodeSymbol)]
    pub fn encode_symbol(&self, esi: u32) -> js_sys::Uint8Array {
        let sym = self.encoder.encode_symbol(esi);
        js_sys::Uint8Array::from(&sym.data[..])
    }
}

/// Pure-JS-side decoding helper mirroring FountainDecoder, exposed so the
/// demo can implement "receiver" in the browser without network code.
#[wasm_bindgen]
pub struct Decoder {
    decoder: FountainDecoder,
    received: u32,
}

#[wasm_bindgen]
impl Decoder {
    #[wasm_bindgen(constructor)]
    pub fn new(payload_len: usize, symbol_size: usize, k: u32, checksum_hex: &str) -> Decoder {
        let checksum = u64::from_str_radix(checksum_hex, 16).expect("valid hex checksum");
        Self {
            decoder: FountainDecoder::new(payload_len, symbol_size, k as usize, checksum),
            received: 0,
        }
    }

    #[wasm_bindgen(js_name = addSymbol)]
    pub fn add_symbol(&mut self, esi: u32, data: &[u8]) -> bool {
        self.received += 1;
        let sym = orbit_fountain::EncodedSymbol {
            esi,
            symbol_size: self.decoder.symbol_size(),
            data: data.to_vec(),
        };
        self.decoder.add_symbol(sym)
    }

    #[wasm_bindgen(js_name = received)]
    pub fn received(&self) -> u32 {
        self.received
    }

    #[wasm_bindgen(js_name = isComplete)]
    pub fn is_complete(&self) -> bool {
        self.decoder.is_complete()
    }

    /// Returns the reconstructed payload (throws if incomplete/checksum bad).
    #[wasm_bindgen(js_name = reconstruct)]
    pub fn reconstruct(&self) -> Result<js_sys::Uint8Array, JsValue> {
        let data = self
            .decoder
            .reconstruct()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(js_sys::Uint8Array::from(&data[..]))
    }
}