# HN first comment (paste as the first comment on your Show HN)

Author here. Orbit-Transfer is a file transfer where the sender keeps emitting
an unbounded stream of encoded symbols and the receiver rebuilds the file from
any K+ε of them — so there's never a "please resend packet #1234" round-trip,
and symbols can arrive from N paths at once, in any order, and it just works.

**Why rateless codes:** the overhead adapts to the actual loss rate. At 0% loss
the systematic phase costs exactly 1.000× (no redundancy at all); at 40%
simulated loss in the WebRTC demo it still decodes. The HDPC layer (RaptorQ-style)
is what keeps it near-optimal at small K.

**The multi-path part:** because symbols are interchangeable, a trivial
round-robin scheduler fans them over N relays and throughput adds up. Bench
(32 MiB, each path capped at 2048 kbps, 3 runs):

```
http      2.12 MiB/s   1.00×
orbit-1   2.63 MiB/s   1.24×
orbit-2   4.41 MiB/s   2.08×
orbit-3   6.02 MiB/s   2.84×
```

Reproducible with `node bench/run-bench.mjs`.

**Quick try (no install):** https://orbit-transfer-theta.vercel.app/webrtc/ —
two tabs, or share the `?room=` link with a friend. The whole codec is Rust
compiled to WASM.

**Ships as:** npm `orbit-transfer-wasm` (encode/decode in the browser),
Docker `dylanondo/orbit-relay` (~78 MB edge relay), plus an IEEEtran paper
(EN + FR) in the repo.

I'm happy to answer questions — especially "why not just use BitTorrent/MPTCP",
"what about security", or "does it beat rsync".

—

Note: I couldn't embed the chart in this comment (HN doesn't render images) —
the numbers above are the takeaway. If anyone wants the SVG: it's in `bench/chart.svg`.