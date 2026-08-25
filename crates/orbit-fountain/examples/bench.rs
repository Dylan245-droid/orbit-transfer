use orbit_fountain::{FountainDecoder, FountainEncoder};
use std::time::Instant;

fn deterministic_payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| ((i * 131) % 251) as u8).collect()
}

fn bench(size: usize, symbol_size: usize, loss: f64) {
    let payload = deterministic_payload(size);
    let encoder = FountainEncoder::new(&payload, symbol_size);
    let k = encoder.k();
    let l = encoder.l();

    // Rateless: pre-encode a pool of 1.5 * L symbols (unbounded ESI).
    let pool_count = (l as f64 * 1.5) as u32 + 10;
    let start = Instant::now();
    let pool: Vec<_> = (0..pool_count).map(|esi| encoder.encode_symbol(esi)).collect();
    let enc_time = start.elapsed();
    let enc_mbps = size as f64 * 8.0 / 1e6 / enc_time.as_secs_f64();

    // Simulate uniform random loss over the pool.
    let feed: Vec<_> = if loss > 0.0 {
        let drop_every = (1.0 / loss) as usize;
        pool.into_iter()
            .enumerate()
            .filter(|(i, _)| i % drop_every != 0)
            .map(|(_, s)| s)
            .collect()
    } else {
        pool
    };

    // Decode by consuming symbols in order until reconstruction completes.
    let mut decoder = FountainDecoder::new(size, symbol_size, k, encoder.checksum());
    let start = Instant::now();
    let mut used = 0usize;
    for sym in &feed {
        used += 1;
        if decoder.add_symbol(sym.clone()) {
            break;
        }
    }
    let dec_time = start.elapsed();
    let recovered = decoder.reconstruct().expect("payload must reconstruct");
    assert_eq!(recovered, payload, "integrity check");

    println!(
        "{size:>10} B | sym {:>7} | loss {loss:>4.0}% | enc {:>8.3}s ({:>9.1} Mb/s) | \
         dec {:>8.3}s | used {:>7}/{:>7} (x{:.3}) | OK",
        symbol_size,
        enc_time.as_secs_f64(),
        enc_mbps,
        dec_time.as_secs_f64(),
        used,
        feed.len(),
        used as f64 / l as f64,
    );
}

fn main() {
    println!("orbit-fountain benchmark (rateless streaming)");
    println!("-------------------------------------------------------------------");
    for &symbol_size in &[4096usize, 65536] {
        for &size in &[1usize << 20, 16usize << 20, 64usize << 20] {
            bench(size, symbol_size, 0.0);
            bench(size, symbol_size, 0.05);
        }
        println!();
    }
}