#!/usr/bin/env python3
"""Deterministic stand-in for Ollama's POST /api/embed."""
import argparse
import hashlib
import json
import math
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DEFAULT_DIMS = 512


def unit_vector(text, dims):
    values = []
    counter = 0
    while len(values) < dims:
        digest = hashlib.sha256(f"{text}\0{counter}".encode("utf-8")).digest()
        for i in range(0, len(digest) - 1, 2):
            if len(values) >= dims:
                break
            raw = int.from_bytes(digest[i : i + 2], "big")
            values.append((raw / 65535.0) * 2.0 - 1.0)
        counter += 1
    norm = math.sqrt(sum(v * v for v in values)) or 1.0
    return [v / norm for v in values]


class Handler(BaseHTTPRequestHandler):
    dims = DEFAULT_DIMS

    def log_message(self, fmt, *args):
        pass

    def _send_json(self, status, payload):
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        if self.path != "/api/embed":
            self._send_json(404, {"error": f"stub-embedder does not implement {self.path}"})
            return
        length = int(self.headers.get("Content-Length", "0") or "0")
        raw = self.rfile.read(length) if length else b"{}"
        try:
            body = json.loads(raw or b"{}")
        except json.JSONDecodeError:
            self._send_json(400, {"error": "invalid json body"})
            return
        inputs = body.get("input", [])
        if isinstance(inputs, str):
            inputs = [inputs]
        embeddings = [unit_vector(text, self.dims) for text in inputs]
        self._send_json(
            200,
            {
                "model": body.get("model", ""),
                "embeddings": embeddings,
                "total_duration": 0,
                "load_duration": 0,
                "prompt_eval_count": len(inputs),
            },
        )

    def do_GET(self):
        if self.path == "/api/tags":
            self._send_json(200, {"models": []})
            return
        self._send_json(404, {"error": f"stub-embedder does not implement {self.path}"})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--dims", type=int, default=DEFAULT_DIMS)
    parser.add_argument("--host", default="127.0.0.1")
    args = parser.parse_args()
    Handler.dims = args.dims
    server = ThreadingHTTPServer((args.host, args.port), Handler)
    server.serve_forever()


if __name__ == "__main__":
    main()
