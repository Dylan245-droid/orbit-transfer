// Test for the automated signaling endpoints of serve.mjs.
//
//   node signaling.test.mjs
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const PORT = 8799;
const ROOT = fileURLToPath(new URL(".", import.meta.url));

// Start serve.mjs on a test port.
const server = spawn(process.execPath, ["serve.mjs"], {
  env: { ...process.env, PORT: String(PORT) },
  cwd: ROOT,
  stdio: "ignore",
});

function req(method, path, body) {
  return new Promise((resolve, reject) => {
    const r = fetch(`http://127.0.0.1:${PORT}${path}`, {
      method,
      body: body ?? undefined,
    });
    r.then(async (res) => resolve({ status: res.status, text: await res.text() }), reject);
  });
}

async function waitUp() {
  for (let i = 0; i < 50; i++) {
    try { await req("GET", "/"); return; } catch { await new Promise((r) => setTimeout(r, 100)); }
  }
  throw new Error("server did not start");
}

try {
  await waitUp();

  // Create room
  const created = await req("POST", "/api/rooms");
  const { roomId } = JSON.parse(created.text);
  if (!roomId) throw new Error("no roomId");

  // Offer not ready before publish
  const before = await req("GET", `/api/rooms/${roomId}/offer`);
  if (before.status !== 404) throw new Error(`expected 404 before offer, got ${before.status}`);

  // Host publishes offer, guest reads it
  const offer = "v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n";
  const putOffer = await req("PUT", `/api/rooms/${roomId}/offer`, offer);
  if (putOffer.status !== 200) throw new Error("publish offer failed");
  const gotOffer = await req("GET", `/api/rooms/${roomId}/offer`);
  if (gotOffer.text !== offer) throw new Error("offer mismatch");

  // Guest publishes answer, host reads it
  const answer = "v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n";
  const putAnswer = await req("PUT", `/api/rooms/${roomId}/answer`, answer);
  if (putAnswer.status !== 200) throw new Error("publish answer failed");
  const gotAnswer = await req("GET", `/api/rooms/${roomId}/answer`);
  if (gotAnswer.text !== answer) throw new Error("answer mismatch");

  // Unknown room
  const missing = await req("GET", "/api/rooms/zzzz/offer");
  if (missing.status !== 404) throw new Error(`expected 404 for unknown room, got ${missing.status}`);

  // Delete
  const del = await req("DELETE", `/api/rooms/${roomId}`);
  if (del.status !== 200) throw new Error("delete failed");
  const after = await req("GET", `/api/rooms/${roomId}/offer`);
  if (after.status !== 404) throw new Error("room should be gone after delete");

  console.log("ok — signaling room offer/answer exchange works");
} finally {
  server.kill();
}