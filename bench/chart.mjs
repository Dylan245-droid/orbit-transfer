// Generates bench/chart.svg from bench.csv (zero-dependency SVG bar chart).
//
//   node chart.mjs
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const rows = readFileSync(path.join(HERE, "bench.csv"), "utf8")
  .trim()
  .split("\n")
  .slice(1)
  .map((l) => {
    const [name, mbps, factor] = l.split(",");
    return { name, mbps: parseFloat(mbps), factor: parseFloat(factor) };
  });

const W = 860;
const H = 500;
const ML = 90; // left margin (y labels)
const MR = 30;
const MT = 70;
const MB = 70;
const plotW = W - ML - MR;
const plotH = H - MT - MB;

const maxMbps = Math.max(...rows.map((r) => r.mbps)) * 1.1;
const xStep = plotW / rows.length;
const barW = xStep * 0.55;
const y = (v) => MT + plotH - (v / maxMbps) * plotH;

const baseline = rows[0].mbps;
const colors = ["#94a3b8", "#2563eb", "#3b82f6", "#60a5fa"];
const labels = { http: "HTTP\n(single path)", "orbit-1": "orbit\n1 relay", "orbit-2": "orbit\n2 relays", "orbit-3": "orbit\n3 relays" };

const parts = [];
parts.push(`<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}">`);
parts.push(`<rect width="${W}" height="${H}" fill="#ffffff"/>`);
parts.push(`<text x="${W / 2}" y="34" text-anchor="middle" font-family="system-ui" font-size="22" font-weight="700" fill="#111">Orbit-Transfer: multi-relay bandwidth aggregation vs a single throttled path</text>`);
parts.push(`<text x="${W / 2}" y="56" text-anchor="middle" font-family="system-ui" font-size="13" fill="#555">32 MiB payload · 2048 kbps cap per path · mean of 3 runs</text>`);

// gridlines + y labels
for (let i = 0; i <= 4; i++) {
  const v = (maxMbps / 4) * i;
  const yy = y(v);
  parts.push(`<line x1="${ML}" y1="${yy}" x2="${W - MR}" y2="${yy}" stroke="${yy === y(0) ? "#444" : "#e2e8f0"}" stroke-width="1"/>`);
  parts.push(`<text x="${ML - 10}" y="${yy + 4}" text-anchor="end" font-family="system-ui" font-size="12" fill="#334">${v.toFixed(1)}</text>`);
}
parts.push(`<text x="${ML - 10}" y="${MT + plotH + 18}" text-anchor="end" font-family="system-ui" font-size="12" fill="#334">MiB/s</text>`);

// baseline (HTTP) dashed line
const bly = y(baseline);
parts.push(`<line x1="${ML}" y1="${bly}" x2="${W - MR}" y2="${bly}" stroke="#ef4444" stroke-width="2" stroke-dasharray="6 4"/>`);
parts.push(`<text x="${W - MR}" y="${bly - 6}" text-anchor="end" font-family="system-ui" font-size="12" fill="#dc2626">HTTP baseline ${baseline.toFixed(2)} MiB/s</text>`);

// bars
rows.forEach((r, i) => {
  const cx = ML + xStep * i + xStep / 2;
  const bh = MT + plotH - y(r.mbps);
  parts.push(`<rect x="${cx - barW / 2}" y="${y(r.mbps)}" width="${barW}" height="${bh}" rx="4" fill="${colors[i]}"/>`);
  parts.push(`<text x="${cx}" y="${y(r.mbps) - 8}" text-anchor="middle" font-family="system-ui" font-size="14" font-weight="700" fill="#111">${r.mbps.toFixed(2)}</text>`);
  parts.push(`<text x="${cx}" y="${y(r.mbps) - 28}" text-anchor="middle" font-family="system-ui" font-size="12" font-weight="600" fill="${r.factor > 1.5 ? "#2563eb" : "#333"}">×${r.factor.toFixed(2)}</text>`);
  const [l1, l2] = (labels[r.name] ?? r.name).split("\n");
  parts.push(`<text x="${cx}" y="${MT + plotH + 24}" text-anchor="middle" font-family="system-ui" font-size="13" fill="#222">${l1}</text>`);
  if (l2) parts.push(`<text x="${cx}" y="${MT + plotH + 42}" text-anchor="middle" font-family="system-ui" font-size="13" fill="#222">${l2}</text>`);
});

parts.push(`</svg>`);
writeFileSync(path.join(HERE, "chart.svg"), parts.join("\n"));
console.log(`chart.svg written: ${rows.length} bars, baseline ${baseline.toFixed(2)} MiB/s, max ${maxMbps.toFixed(2)} MiB/s`);