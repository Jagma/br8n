import { spawn, spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, cpSync, writeFileSync, rmSync, readdirSync, symlinkSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { startOllamaStub } from "./ollama-stub.mjs";

const MODEL = "qwen3-embedding:0.6b";
const HERE = new URL(".", import.meta.url).pathname;

function defaultBinary() {
  const meta = spawnSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
    cwd: resolve(HERE, "../.."),
    encoding: "utf8",
  });
  if (meta.status !== 0) throw new Error(`cargo metadata failed: ${meta.stderr}`);
  return join(JSON.parse(meta.stdout).target_directory, "debug", "br8n");
}

export class Sandbox {
  static async create({ indexed = true } = {}) {
    const s = new Sandbox();
    s.indexed = indexed;
    s.binary = process.env.BR8N_BIN ?? defaultBinary();
    s.root = mkdtempSync(join(tmpdir(), "br8n-e2e-"));
    s.home = join(s.root, "home");
    s.corpus = join(s.root, "corpus");
    s.configDir = join(s.home, ".config", "br8n");
    mkdirSync(s.configDir, { recursive: true });
    cpSync(join(HERE, "corpus"), s.corpus, { recursive: true });
    s.ollama = await startOllamaStub(MODEL);
    s.writeConfig();
    return s;
  }

  corpusDocuments() {
    return readdirSync(this.corpus, { recursive: true }).filter((f) => String(f).endsWith(".md")).length;
  }

  get configPath() {
    return join(this.configDir, "config.toml");
  }

  writeConfig(extra = "") {
    const sources = this.indexed ? `"${this.corpus}"` : "";
    writeFileSync(
      this.configPath,
      `sources = [${sources}]\nindex_transcripts = false\n\n[embed]\nollama_url = "${this.ollama.url}"\n${extra}`,
    );
  }

  get dataDir() {
    return join(this.home, ".local", "share", "br8n");
  }

  placeBinary() {
    const bin = join(this.dataDir, "bin", "br8n");
    mkdirSync(join(this.dataDir, "bin"), { recursive: true });
    if (!existsSync(bin)) symlinkSync(this.binary, bin);
    return bin;
  }

  pretendInstalled(agentDir) {
    mkdirSync(join(this.home, agentDir), { recursive: true });
  }

  env() {
    return {
      ...process.env,
      HOME: this.home,
      XDG_CONFIG_HOME: join(this.home, ".config"),
      XDG_DATA_HOME: join(this.home, ".local", "share"),
      BR8N_CONFIG: this.configPath,
      CODEX_HOME: join(this.home, ".codex"),
      PATH: process.env.PATH,
    };
  }

  run(...args) {
    return new Promise((resolveRun, reject) => {
      const child = spawn(this.binary, args, { env: this.env() });
      let stdout = "";
      let stderr = "";
      child.stdout.on("data", (c) => (stdout += c));
      child.stderr.on("data", (c) => (stderr += c));
      const timer = setTimeout(() => child.kill(), 300_000);
      child.on("exit", (code) => {
        clearTimeout(timer);
        if (code === 0) resolveRun(stdout);
        else reject(new Error(`br8n ${args.join(" ")} exited ${code}\n${stdout}\n${stderr}`));
      });
    });
  }

  startDashboard() {
    return new Promise((resolveUrl, reject) => {
      this.dashboard = spawn(this.binary, ["dashboard", "--port", "0", "--no-open"], { env: this.env() });
      let out = "";
      const onData = (chunk) => {
        out += chunk;
        const m = out.match(/http:\/\/127\.0\.0\.1:\d+/);
        if (m) {
          this.url = m[0];
          resolveUrl(m[0]);
        }
      };
      this.dashboard.stdout.on("data", onData);
      this.dashboard.stderr.on("data", onData);
      this.dashboard.on("exit", (code) => reject(new Error(`dashboard exited ${code}: ${out}`)));
    });
  }

  close() {
    this.dashboard?.kill();
    this.ollama?.close();
    if (!process.env.BR8N_E2E_KEEP) rmSync(this.root, { recursive: true, force: true });
  }
}
