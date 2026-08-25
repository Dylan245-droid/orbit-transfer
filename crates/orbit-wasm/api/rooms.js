// Vercel serverless function backing the WebRTC signaling room API.
// Exact path /api/rooms, with subpaths delivered via the vercel.json rewrite
// as ?path=<rest>. Endpoints:
//
//   POST   /api/rooms              -> { roomId }
//   PUT    /api/rooms/:id/offer    (body: SDP text)
//   GET    /api/rooms/:id/offer    -> SDP text or 404
//   PUT    /api/rooms/:id/answer   (body: SDP text)
//   GET    /api/rooms/:id/answer   -> SDP text or 404
//   DELETE /api/rooms/:id
//
// Rooms are kept in-memory (warm instances) — fine for a demo where the two
// peers join within seconds. TTL of 30 minutes.
const TTL_MS = 30 * 60 * 1000;
const crypto = require("crypto");

function rooms() {
  if (!globalThis.__orbitRooms) globalThis.__orbitRooms = new Map();
  const m = globalThis.__orbitRooms;
  const now = Date.now();
  for (const [id, r] of m) if (now - r.createdAt > TTL_MS) m.delete(id);
  return m;
}

function send(res, status, body, type) {
  res.statusCode = status;
  res.setHeader("content-type", type ?? "application/json");
  res.end(body);
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    req.on("data", (c) => {
      size += c.length;
      if (size > 4 * 1024 * 1024) reject(new Error("body too large"));
      else chunks.push(c);
    });
    req.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    req.on("error", reject);
  });
}

export default async function handler(req, res) {
  const path = req.query.path ?? "";
  const parts = path.split("/").filter(Boolean);
  const method = req.method;

  if (parts.length === 0 && method === "POST") {
    const id = crypto.randomBytes(4).toString("hex");
    rooms().set(id, { offer: null, answer: null, createdAt: Date.now() });
    return send(res, 200, JSON.stringify({ roomId: id }));
  }

  if (parts.length === 1 && method === "DELETE") {
    rooms().delete(parts[0]);
    return send(res, 200, JSON.stringify({ ok: true }));
  }

  if (parts.length === 2) {
    const [roomId, field] = parts;
    if (field !== "offer" && field !== "answer") {
      return send(res, 404, JSON.stringify({ error: "room not found" }));
    }
    const room = rooms().get(roomId);
    if (!room) return send(res, 404, JSON.stringify({ error: "room not found" }));
    if (method === "GET") {
      const v = room[field];
      return v
        ? send(res, 200, v, "text/plain; charset=utf-8")
        : send(res, 404, JSON.stringify({ error: `${field} not ready` }));
    }
    if (method === "PUT") {
      const body = await readBody(req).catch(() => "");
      if (!body.trim()) return send(res, 400, JSON.stringify({ error: "empty body" }));
      room[field] = body;
      return send(res, 200, JSON.stringify({ ok: true }));
    }
    return send(res, 405, JSON.stringify({ error: "method not allowed" }));
  }

  send(res, 404, JSON.stringify({ error: "not found" }));
}