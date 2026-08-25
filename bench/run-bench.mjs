// Comparative benchmark: Orbit-Transfer multi-relay aggregation vs a single
// throttled HTTP path.
//
//   node run-bench.mjs [KBPS] [SIZE_MB] [RUNS]
//
// Cases (each constrained path capped at KBPS):
//   http        single throttled HTTP server           -> baseline single path
//   orbit 1     one throttled relay                    -> should match http
//   orbit 2     two throttled relays, round-robin      -> ~2x
//   orbit 3     three throttled relays, round-robin    -> ~3x
//
// Orbit runs relay-only (`--no-p2p`) so the direct path never confounds the
// relay aggregation. Prints a Markdown table and writes bench.csv.
import { spawn } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import net from "node:net";
import { fileURLToPath } from "node:url";

const KBPS = parseInt(process.argv[2] ?? "2048", 10);
const SIZE_MB = parseInt(process.argv[3] ?? "32", 10);
const RUNS = parseInt(process.argv[4] ?? "3", 10);

const BENCH_DIR = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(BENCH_DIR, "..");
const ORBIT = path.join(ROOT, "target", "release", "orbit.exe");
const RELAY = path.join(ROOT, "target", "release", "orbit-relay.exe");
const HTTP_SERVER = path.join(BENCH_DIR, "http-server.mjs");

const tmp = mkdtempSync(path.join(tmpdir(), "orbit-bench-"));
const file = path.join(tmp, "payload.bin");
const sizeBytes = SIZE_MB * 1024 * 1024;
const payload = Buffer.allocUnsafe(sizeBytes);
for (let i = 0; i < sizeBytes; i++) payload[i] = (i * 131) % 251;
writeFileSync(file, payload);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Finds a free TCP port (bind :0, note it, close, return). Small race, fine
// for a benchmark.
function freePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.listen(0, "127.0.0.1", () => {
      const port = srv.address().port;
      srv.close(() => resolve(port));
    });
    srv.on("error", reject);
  });
}

function spawnAsync(cmd, args) {
  return new Promise((resolve, reject) => {
    const p = spawn(cmd, args, { stdio: ["ignore", "pipe", "pipe"] });
    let out = "";
    let err = "";
    p.stdout.on("data", (d) => (out += d));
    p.stderr.on("data", (d) => (err += d));
    p.on("error", reject);
    p.on("close", (code) => resolve({ code, out, err }));
  });
}

async function spawnRelay(kbps) {
  const port = await freePort();
  const p = spawn(RELAY, ["serve", "--addr", `127.0.0.1:${port}`, "--throttle-kbps", String(kbps)], {
    stdio: ["ignore", "ignore", "ignore"],
  });
  await sleep(150); // allow bind
  return { proc: p, url: `ws://127.0.0.1:${port}` };
}

async function caseHttp() {
  // HTTP server prints `READY <port>` via console.log (flushed on pipe).
  const serverProc = await new Promise((resolve, reject) => {
    const p = spawn(process.execPath, [HTTP_SERVER, file, String(KBPS)], { stdio: ["ignore", "pipe", "ignore"] });
    let out = "";
    const t = setTimeout(() => { p.kill(); reject(new Error("http server did not print READY: " + out)); }, 15000);
    p.stdout.on("data", (d) => {
      out += d;
      const m = out.match(/READY (\d+)/);
      if (m) { clearTimeout(t); resolve({ proc: p, port: m[1] }); }
    });
    p.on("error", reject);
  });
  const port = serverProc.port;
  const url = `http://127.0.0.1:${port}/payload.bin`;
  const start = Date.now();
  const res = await fetch(url);
  let got = 0;
  for await (const chunk of res.body) got += chunk.length;
  const elapsed = (Date.now() - start) / 1000;
  serverProc.proc.kill();
  return { bytes: got, elapsed, mbps: got / 1048576 / elapsed };
}

async function caseOrbit(nRelays, runIdx) {
  const relays = [];
  try {
    const urls = [];
    for (let i = 0; i < nRelays; i++) {
      const { proc, url } = await spawnRelay(KBPS);
      relays.push(proc);
      urls.push(url);
    }
    const session = 0x100000 + (runIdx * 1000) + nRelays * 100 + Math.floor(Math.random() * 100);
    const outFile = path.join(tmp, `out-${nRelays}-${runIdx}.bin`);
    const args = (side) => [
      side === "send" ? "send" : "receive",
      side === "send" ? file : outFile,
      ...urls.flatMap((u) => ["--relay", u]),
      "--session",
      String(session),
      ...(side === "receive" ? ["--no-p2p"] : []),
    ];
    const [sender, receiver] = await Promise.all([
      spawnAsync(ORBIT, args("send")),
      spawnAsync(ORBIT, args("receive")),
    ]);
    const m = sender.out.match(/throughput:\s+([\d.]+)/);
    if (!m) throw new Error(`no throughput line: ${sender.out}\n${sender.err}`);
    return { elapsed: parseFloat((/complete in ([\d.]+)s/.exec(sender.out) ?? [0, 0])[1]), mbps: parseFloat(m[1]) };
  } finally {
    for (const r of relays) r.kill();
  }
}

function mean(xs) {
  return xs.reduce((a, b) => a + b, 0) / xs.length;
}

const results = {};
const cases = [["http", 0], ["orbit-1", 1], ["orbit-2", 2], ["orbit-3", 3]];

console.log(`orbit bench · ${SIZE_MB} MiB payload · ${KBPS} kbps per path · ${RUNS} runs\n`);
for (const [name, n] of cases) {
  const runs = [];
  for (let i = 0; i < RUNS; i++) {
    const r = n === 0 ? await caseHttp() : await caseOrbit(n, i);
    runs.push(r);
    console.log(`  ${name.padEnd(8)} run ${i + 1}: ${r.mbps.toFixed(2)} MiB/s (${r.elapsed.toFixed(2)}s)`);
  }
  results[name] = { runs, mbps: mean(runs.map((r) => r.mbps)), elapsed: mean(runs.map((r) => r.elapsed)) };
  console.log();
}

const base = results["http"].mbps;
console.log("| Case | MiB/s (mean) | Factor vs HTTP |");
console.log("|------|--------------|----------------|");
for (const [name] of cases) {
  console.log(`| ${name} | ${results[name].mbps.toFixed(2)} | ${(results[name].mbps / base).toFixed(2)}x |`);
}

const csv = ["case,mi_b_s,factor", ...cases.map(([name]) => `${name},${results[name].mbps.toFixed(3)},${(results[name].mbps / base).toFixed(3)}`)];
writeFileSync(path.join(BENCH_DIR, "bench.csv"), csv.join("\n"));
console.log(`\nbench.csv written (${KBPS} kbps, ${SIZE_MB} MiB, ${RUNS} runs)`);

rmSync(tmp, { recursive: true, force: true });
