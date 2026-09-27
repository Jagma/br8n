import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { assert } from "../helpers.mjs";

const call = (page, path, body) =>
  page.evaluate(
    async (path, body) => {
      const init = body === undefined ? {} : { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) };
      const r = await fetch(path, init);
      return { status: r.status, json: await r.json() };
    },
    path,
    body,
  );

export const tests = {
  "the agents api lists every agent with its snippets": async ({ page }) => {
    const { status, json } = await call(page, "/api/agents");
    assert(status === 200, `GET /api/agents answered ${status}`);
    const ids = json.agents.map((a) => a.id);
    assert(
      JSON.stringify(ids) === JSON.stringify(["claude-code", "codex", "claude-desktop", "cursor", "gemini"]),
      `unexpected agents: ${ids}`,
    );
    for (const a of json.agents) {
      assert(typeof a.status.state === "string", `${a.id} has no status`);
      assert(a.capabilities.mcp === true, `${a.id} lacks mcp`);
    }
    assert(json.snippets.mcp_json.includes('"mcpServers"'), "no JSON snippet");
    assert(json.snippets.codex_toml.includes("[mcp_servers.br8n]"), "no TOML snippet");
  },

  "cursor connects and disconnects inside the sandbox home": async ({ page, sandbox }) => {
    const file = join(sandbox.home, ".cursor", "mcp.json");
    const connected = await call(page, "/api/agents/connect", { id: "cursor" });
    assert(connected.status === 200, `connect answered ${connected.status}: ${JSON.stringify(connected.json)}`);
    assert(connected.json.change.files[0] === file, `wrote ${connected.json.change.files}`);
    assert(connected.json.agent.status.state !== "not_connected", "still not connected");
    const entry = JSON.parse(readFileSync(file, "utf8")).mcpServers.br8n;
    assert(entry.command.endsWith("/br8n/bin/br8n") && entry.args[0] === "mcp", `entry: ${JSON.stringify(entry)}`);

    const again = await call(page, "/api/agents/connect", { id: "cursor" });
    assert(again.json.change.files.length === 0, "a second connect changed files");

    const gone = await call(page, "/api/agents/disconnect", { id: "cursor" });
    assert(gone.status === 200, `disconnect answered ${gone.status}`);
    assert(gone.json.agent.status.state === "not_connected", `state after disconnect: ${gone.json.agent.status.state}`);
    assert(existsSync(file) && !JSON.parse(readFileSync(file, "utf8")).mcpServers, "entry left behind");
  },

  "an unknown agent is a 404": async ({ page }) => {
    const r = await call(page, "/api/agents/connect", { id: "vim" });
    assert(r.status === 404, `answered ${r.status}`);
  },
};

export const allowErrors = [/status of 404/];
