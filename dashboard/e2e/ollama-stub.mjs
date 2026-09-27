import http from "node:http";
import { createHash } from "node:crypto";

const DIMENSIONS = 1024;

function tokens(text) {
  return text.toLowerCase().match(/[a-z0-9]+/g) ?? [];
}

export function embed(text) {
  const v = new Float32Array(DIMENSIONS);
  for (const t of tokens(text)) {
    const h = createHash("sha256").update(t).digest();
    v[h.readUInt32LE(0) % DIMENSIONS] += 1;
    v[h.readUInt32LE(4) % DIMENSIONS] += 0.5;
  }
  const norm = Math.hypot(...v) || 1;
  return Array.from(v, (x) => x / norm);
}

function readJson(req) {
  return new Promise((resolve) => {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      try {
        resolve(JSON.parse(body || "{}"));
      } catch {
        resolve({});
      }
    });
  });
}

export function startOllamaStub(model) {
  const server = http.createServer(async (req, res) => {
    const send = (status, value) => {
      res.writeHead(status, { "content-type": "application/json" });
      res.end(JSON.stringify(value));
    };
    if (req.method === "POST" && req.url === "/api/embed") {
      const body = await readJson(req);
      const inputs = Array.isArray(body.input) ? body.input : [body.input ?? ""];
      return send(200, { model: body.model, embeddings: inputs.map(embed) });
    }
    if (req.url === "/api/tags") {
      return send(200, { models: [{ name: model, model }] });
    }
    if (req.url === "/api/ps") {
      return send(200, { models: [] });
    }
    if (req.method === "POST" && req.url === "/api/show") {
      return send(200, { model_info: { "qwen3.embedding_length": DIMENSIONS } });
    }
    send(404, { error: `stub has no ${req.method} ${req.url}` });
  });
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      resolve({ url: `http://127.0.0.1:${server.address().port}`, close: () => server.close() });
    });
  });
}
