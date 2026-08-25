// Throttled HTTP file server — the "single constrained path" baseline for the
// comparative benchmark. Mirrors the relay's self-correcting token bucket so
// the comparison is fair: same egress cap, same pacing mechanism.
//
//   node http-server.mjs <file> [kbps]
import http from "node:http";
import { stat, open } from "node:fs/promises";

class RateLimiter {
  constructor(kbps) {
    this.rate = kbps * 1024; // bytes per second
    this.tokens = 0;
    this.last = Date.now();
    this.cap = this.rate * 0.01; // ~10 ms of credit, prevents long-idle burst
  }
  async throttle(n) {
    const now = Date.now();
    this.tokens = Math.min(this.tokens + ((now - this.last) / 1000) * this.rate, this.cap);
    this.last = now;
    if (this.tokens >= n) {
      this.tokens -= n;
      return;
    }
    const wait = (n - this.tokens) / this.rate;
    this.tokens = 0;
    if (wait > 0) await new Promise((r) => setTimeout(r, wait * 1000));
  }
}

const file = process.argv[2];
const kbps = parseInt(process.argv[3] ?? "2048", 10);
if (!file) {
  console.error("usage: node http-server.mjs <file> [kbps]");
  process.exit(1);
}

const CHUNK = 256 * 1024;

const server = http.createServer(async (req, res) => {
  try {
    const info = await stat(file);
    if (!info.isFile()) {
      res.writeHead(404);
      return res.end("not a file");
    }
    const limiter = new RateLimiter(kbps);
    res.writeHead(200, {
      "content-type": "application/octet-stream",
      "content-length": info.size,
      "cache-control": "no-store",
    });
    const fd = await open(file, "r");
    const buf = Buffer.alloc(CHUNK);
    let pos = 0;
    while (pos < info.size) {
      const { bytesRead } = await fd.read(buf, 0, CHUNK, pos);
      if (bytesRead === 0) break;
      await limiter.throttle(bytesRead);
      if (!res.write(buf.subarray(0, bytesRead))) {
        await new Promise((r) => res.once("drain", r));
      }
      pos += bytesRead;
    }
    res.end();
    await fd.close();
  } catch (e) {
    console.error("http-server error:", e);
    if (!res.headersSent) res.writeHead(500);
    res.end(String(e));
  }
});

server.listen(0, "127.0.0.1", () => {
  console.log(`READY ${server.address().port}`);
});