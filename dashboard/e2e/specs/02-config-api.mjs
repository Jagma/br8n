import { readFileSync } from "node:fs";
import { join } from "node:path";
import { assert } from "../helpers.mjs";

async function call(page, path, body) {
  return page.evaluate(
    async (path, body) => {
      const init = body === undefined
        ? {}
        : { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) };
      const r = await fetch(path, init);
      return { status: r.status, json: await r.json() };
    },
    path,
    body,
  );
}

export const allowErrors = [/status of 409 \(Conflict\)/];

export const tests = {
  "the config API reads the sandbox config": async ({ page, sandbox }) => {
    const { status, json } = await call(page, "/api/config");
    assert(status === 200, `GET /api/config answered ${status}`);
    assert(json.exists === true, "the sandbox wrote a config file");
    assert(/^[0-9a-f]{64}$/.test(json.etag), `etag is a sha256: ${json.etag}`);
    assert(json.effective.embed.ollama_url === sandbox.ollama.url, "effective carries the stub URL");
    assert(json.file.index_transcripts === false, "file carries only what the sandbox wrote");
    assert(json.file.hook === undefined, "hook is not in the file");
    assert(json.defaults.hook.threshold === 0.66, `default hook threshold, got ${json.defaults.hook.threshold}`);
    assert(json.errors.length === 0, `the sandbox config is clean: ${JSON.stringify(json.errors)}`);
    assert(json.secrets["embed.token"] === "unset", "no token is configured");
  },

  "a save from the dashboard round-trips through config.toml": async ({ page, sandbox }) => {
    const before = readFileSync(sandbox.configPath, "utf8");
    const { json: state } = await call(page, "/api/config");

    const check = await call(page, "/api/config/check", { set: { "hook.threshold": 3 } });
    assert(check.status === 200 && check.json.blocking[0]?.path === "hook.threshold", JSON.stringify(check.json));
    assert(readFileSync(sandbox.configPath, "utf8") === before, "a check writes nothing");

    const saved = await call(page, "/api/config", { etag: state.etag, set: { "hook.threshold": 0.7 } });
    assert(saved.status === 200, `save answered ${saved.status}: ${JSON.stringify(saved.json)}`);
    const written = readFileSync(sandbox.configPath, "utf8");
    assert(written.startsWith(before.trimEnd()), `the rest of the file survives:\n${written}`);
    assert(written.includes("[hook]\nthreshold = 0.7"), `the change is on disk:\n${written}`);
    assert(saved.json.effective.hook.threshold === 0.7, "the answer is the new state");

    const stale = await call(page, "/api/config", { etag: state.etag, set: { "hook.threshold": 0.5 } });
    assert(stale.status === 409 && stale.json.etag === saved.json.etag, `a stale etag is a conflict: ${stale.status}`);

    const reset = await call(page, "/api/config", { etag: saved.json.etag, unset: ["hook.threshold"] });
    assert(reset.status === 200 && reset.json.effective.hook.threshold === 0.66, "unset restores the default");
  },

  "re-index starts a detached br8n index that runs to completion": async ({ page, sandbox }) => {
    const { status, json } = await call(page, "/api/index", {});
    assert(status === 202 && json.started === true && json.pid > 0, `index answered ${status}: ${JSON.stringify(json)}`);
    const deadline = Date.now() + 120_000;
    const alive = () => {
      try {
        process.kill(json.pid, 0);
        return true;
      } catch {
        return false;
      }
    };
    while (alive() && Date.now() < deadline) await new Promise((r) => setTimeout(r, 250));
    assert(!alive(), "the index finished within two minutes");
    const log = readFileSync(join(sandbox.home, ".local/share/br8n/db.log"), "utf8");
    assert(/index/i.test(log), `the child logged to db.log:\n${log.slice(-400)}`);
  },

  "testing the embedder reaches the stub": async ({ page }) => {
    const { status, json } = await call(page, "/api/embed/test", {});
    assert(status === 200, `embed test answered ${status}`);
    assert(json.ok === true && json.backend === "ollama" && json.dimensions > 0, JSON.stringify(json));
  },

  "a full re-index runs to completion and reports how it ended": async ({ page, sandbox }) => {
    const { status, json } = await call(page, "/api/index", { reindex: true });
    assert(status === 202 && json.reindex === true, `index answered ${status}: ${JSON.stringify(json)}`);
    const deadline = Date.now() + 120_000;
    let run = null;
    while (Date.now() < deadline) {
      run = (await call(page, "/api/index")).json.run;
      if (run?.pid === json.pid && run.finished) break;
      await new Promise((r) => setTimeout(r, 250));
    }
    assert(run?.finished === true, `the re-index finished within two minutes: ${JSON.stringify(run)}`);
    assert(run.ok === true && run.reindex === true && run.code === 0, `it succeeded: ${JSON.stringify(run)}`);
    const stats = (await call(page, "/api/stats")).json;
    assert(stats.documents === sandbox.corpusDocuments(), `the rebuilt index holds the corpus: ${stats.documents}`);
  },
};
