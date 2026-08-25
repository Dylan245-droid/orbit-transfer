/**
 * Orbit-Transfer · TypeScript bindings for the WASM build of
 * `orbit-fountain` (rateless erasure codes with LDPC precoding).
 *
 * The package wraps the wasm-bindgen output produced by
 * `npm run build:wasm` (wasm-pack, `--target web`).
 *
 * @packageDocumentation
 */

import init, {
  Decoder as WasmDecoder,
  Fountain as WasmFountain,
  type InitInput,
} from "../pkg/orbit_wasm";

let ready: Promise<void> | null = null;

/**
 * Initializes the WASM module. Idempotent: calling it multiple times is
 * free. When `module_or_path` is omitted, the wasm binary is fetched from
 * the same origin (for bundlers, resolve it via `?url` imports or place
 * `orbit_wasm_bg.wasm` next to the bundle).
 */
export function loadOrbit(
  module_or_path?: InitInput | Promise<InitInput>,
): Promise<void> {
  ready ??= (async () => {
    await init({ module_or_path });
  })();
  return ready;
}

/** Encoding half of a fountain transfer. */
export class OrbitEncoder {
  private readonly inner: WasmFountain;

  /** Builds an encoder over `payload` with the given symbol size. */
  constructor(payload: Uint8Array, symbolSize: number) {
    this.inner = new WasmFountain(payload, symbolSize);
  }

  /** Number of source symbols the payload is split into. */
  get k(): number {
    return this.inner.k();
  }

  /** Systematic phase length (K + S, S = LDPC precoding overhead). */
  get l(): number {
    return this.inner.l();
  }

  get payloadLength(): number {
    return this.inner.payloadLen();
  }

  get symbolSize(): number {
    return this.inner.symbolSize();
  }

  /** XXH3 checksum of the payload (hex), used by the decoder for integrity. */
  get checksumHex(): string {
    return this.inner.checksumHex();
  }

  /**
   * Produces the symbol with the given index. The encoder is truly
   * rateless: any `esi >= 0` is valid, so you can emit as many distinct
   * symbols as the link loss demands.
   */
  encodeSymbol(esi: number): Uint8Array {
    return this.inner.encodeSymbol(esi).slice();
  }
}

/** Decoding half of a fountain transfer. */
export class OrbitDecoder {
  private readonly inner: WasmDecoder;

  /**
   * @param payloadLength size in bytes of the original payload
   * @param symbolSize symbol size used by the encoder
   * @param k number of source symbols
   * @param checksumHex XXH3 checksum as hex (from `OrbitEncoder.checksumHex`)
   */
  constructor(
    payloadLength: number,
    symbolSize: number,
    k: number,
    checksumHex: string,
  ) {
    this.inner = new WasmDecoder(payloadLength, symbolSize, k, checksumHex);
  }

  /**
   * Feeds a symbol into the decoder. Duplicates and out-of-order arrivals
   * are harmless (order never matters for a rateless decoder).
   * Returns `true` when the payload is fully decoded.
   */
  addSymbol(esi: number, data: Uint8Array): boolean {
    return this.inner.addSymbol(esi, data);
  }

  /** Number of distinct symbols fed so far. */
  get received(): number {
    return this.inner.received();
  }

  get isComplete(): boolean {
    return this.inner.isComplete();
  }

  /** Reconstructed payload. Throws if incomplete or checksum mismatch. */
  reconstruct(): Uint8Array {
    return this.inner.reconstruct().slice();
  }
}

/** Options for {@link simulateTransfer}. */
export interface SimulateOptions {
  /** Symbol size in bytes (default 4096). */
  symbolSize?: number;
  /** Simulated packet-loss probability per symbol, 0..1 (default 0). */
  loss?: number;
}

/** Outcome of {@link simulateTransfer}. */
export interface SimulateResult {
  /** Symbols emitted by the sender. */
  sent: number;
  /** Symbols that actually reached the decoder. */
  fed: number;
  /** Symbols the decoder needed (K ≤ fed). */
  needed: number;
  /** Decoded payload (byte-identical to the input). */
  decoded: Uint8Array;
  /** Decoding overhead factor: fed / K. */
  overhead: number;
}

/**
 * Runs a complete end-to-end transfer in memory: encodes `payload`,
 * drops `loss`% of the symbols, decodes what remains and verifies the
 * checksum. This is the same math the native CLI runs over the network.
 */
export function simulateTransfer(
  payload: Uint8Array,
  options: SimulateOptions = {},
): SimulateResult {
  const symbolSize = options.symbolSize ?? 4096;
  const loss = options.loss ?? 0;

  const encoder = new OrbitEncoder(payload, symbolSize);
  const k = encoder.k;
  const decoder = new OrbitDecoder(
    encoder.payloadLength,
    encoder.symbolSize,
    k,
    encoder.checksumHex,
  );

  let sent = 0;
  let esi = 0;
  let complete = false;
  // LDPC precoding needs L = K + S symbols in the worst case; emit enough
  // to survive the loss rate with high probability.
  const budget = Math.ceil(encoder.l * (1 + loss * 1.5)) + 10;
  while (esi < budget && !complete) {
    if (Math.random() >= loss) {
      sent += 1;
      const data = encoder.encodeSymbol(esi);
      complete = decoder.addSymbol(esi, data);
    }
    esi += 1;
  }

  return {
    sent,
    fed: decoder.received,
    needed: k,
    decoded: decoder.reconstruct(),
    overhead: decoder.received / k,
  };
}