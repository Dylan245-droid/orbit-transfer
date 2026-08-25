// Minimal zero-dependency static server for the WebRTC demo, plus a tiny
// in-memory signaling service so the SDP offer/answer can be exchanged over
// HTTP instead of copy/paste.
//
// Signaling API (same origin as the demo):
//   POST /api/rooms                -> { roomId }
//   PUT  /api/rooms/:id/offer      (body: SDP text)  [host]
//   GET  /api/rooms/:id/offer      -> SDP text or 404 [guest]
//   PUT  /api/rooms/:id/answer     (body: SDP text)  [guest]
//   GET  /api/rooms/:id/answer     -> SDP text or 404 [host]
//   DELETE /api/rooms/:id                             [any]
//
//   node serve.mjs                 -> http://127.0.0.1:8787/webrtc/index.html
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";
import { randomBytes } from "node:crypto";

const ROOT = fileURLToPath(new URL("..", import.meta.url)); // crates/orbit-wasm/
const PORT = process.env.PORT ?? 8787;

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".wasm": "application/wasm",
  ".json": "application/json",
  ".css": "text/css",
  ".map": "application/json",
};

// In-memory signaling rooms: { roomId: { offer, answer, createdAt } }.
// Rooms expire after 30 minutes.
const rooms = new Map();
const ROOM_TTL_MS = 30 * 60 * 1000;
setInterval(() => {
  const now = Date.now();
  for (const [id, room] of rooms) {
    if (now - room.createdAt > ROOM_TTL_MS) rooms.delete(id);
  }
}, 60_000).unref();

function json(res, status, body) {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
}

async function readBody(req, limit = 4 * 1024 * 1024) {
  const chunks = [];
  let size = 0;
  for await (const chunk of req) {
    size += chunk.length;
    if (size > limit) throw new Error("body too large");
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString("utf8");
}

function handleApi(url, req, res) {
  const m = url.pathname.match(/^\/api\/rooms(?:\/([A-Za-z0-9]+))?(?:\/(offer|answer))?$/);
  const roomId = m?.[1];
  const field = m?.[2];
  const method = req.method;

  if (url.pathname === "/api/rooms" && method === "POST") {
    const id = randomBytes(4).toString("hex");
    rooms.set(id, { offer: null, answer: null, createdAt: Date.now() });
    return json(res, 200, { roomId: id });
  }

  if (!roomId) return json(res, 404, { error: "room not found" });
  const room = rooms.get(roomId);
  if (!room) return json(res, 404, { error: "room not found" });

  if (method === "DELETE" && !field) {
    rooms.delete(roomId);
    return json(res, 200, { ok: true });
  }

  if (field && method === "GET") {
    const v = room[field];
    if (!v) return json(res, 404, { error: `${field} not ready` });
    res.writeHead(200, { "content-type": "text/plain; charset=utf-8" });
    return res.end(v);
  }

  if (field && method === "PUT") {
    return readBody(req).then((body) => {
      if (!body.trim()) return json(res, 400, { error: "empty body" });
      room[field] = body;
      json(res, 200, { ok: true });
    });
  }

  json(res, 405, { error: "method not allowed" });
}

const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url, `http://${req.headers.host}`);
    if (url.pathname.startsWith("/api/")) return handleApi(url, req, res);
    let path = normalize(decodeURIComponent(url.pathname));
    if (path === "/" || path.endsWith("/")) path += "index.html";
    if (path.startsWith("..")) throw new Error("bad path");
    const file = join(ROOT, path);
    const body = await readFile(file);
    res.writeHead(200, {
      "content-type": MIME[extname(file)] ?? "application/octet-stream",
      "cache-control": "no-store",
      "cross-origin-embedder-policy": "require-corp",
      "cross-origin-opener-policy": "same-origin",
    });
    res.end(body);
  } catch {
    res.writeHead(404, { "content-type": "text/plain" });
    res.end("not found");
  }
});

server.listen(PORT, "127.0.0.1", () => {
  console.log(`orbit-webrtc demo: http://127.0.0.1:${PORT}/webrtc/index.html`);
});