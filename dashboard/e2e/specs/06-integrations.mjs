import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { assert, clickButton, openTab, reloadPage, waitForText } from "../helpers.mjs";

const AGENTS = ["claude-code", "codex", "claude-desktop", "cursor", "gemini"];

async function openIntegrations(page) {
  await openTab(page, "Integrations");
  await page.waitForSelector("[data-testid=integrations]", { timeout: 20_000 });
}

function card(page, id) {
  return page.$eval(`[data-agent=${id}]`, (el) => ({
    text: el.innerText,
    state: el.querySelector("[data-testid=agent-status]")?.getAttribute("data-state"),
  }));
}

async function waitForState(page, id, state) {
  await page.waitForFunction(
    (id, state) => document.querySelector(`[data-agent=${id}] [data-testid=agent-status]`)?.getAttribute("data-state") === state,
    { timeout: 20_000 },
    id,
    state,
  );
}

export const tests = {
  "the tab lists all five agents": async ({ page }) => {
    await openIntegrations(page);
    const ids = await page.evaluate(() =>
      [...document.querySelectorAll("[data-agent], [data-missing]")].map((el) => el.getAttribute("data-agent") ?? el.getAttribute("data-missing")),
    );
    assert(JSON.stringify([...ids].sort()) === JSON.stringify([...AGENTS].sort()), `listed: ${ids}`);
  },

  "cursor connects from its card and shows the files it wrote": async ({ page, sandbox }) => {
    sandbox.pretendInstalled(".cursor");
    sandbox.placeBinary();
    await reloadPage(page);
    await openIntegrations(page);
    const before = await card(page, "cursor");
    assert(before.state === "not_connected", `cursor starts ${before.state}`);
    assert(before.text.includes("MCP tools"), `no capability chip:\n${before.text}`);
    await clickButton(page, "Connect", "data-agent='cursor'");
    await page.waitForSelector("[data-agent=cursor] [data-testid=agent-change]", { timeout: 20_000 });
    await waitForState(page, "cursor", "connected");
    const after = await card(page, "cursor");
    const file = join(sandbox.home, ".cursor", "mcp.json");
    assert(/files written/i.test(after.text) && after.text.includes(file), `the change does not name ${file}:\n${after.text}`);
    const entry = JSON.parse(readFileSync(file, "utf8")).mcpServers.br8n;
    assert(entry.args[0] === "mcp", `entry: ${JSON.stringify(entry)}`);
  },

  "disconnecting cursor asks first and then removes br8n": async ({ page, sandbox }) => {
    await openIntegrations(page);
    await waitForState(page, "cursor", "connected");
    await clickButton(page, "Disconnect", "data-agent='cursor'");
    await waitForText(page, "Remove br8n from Cursor?");
    const file = join(sandbox.home, ".cursor", "mcp.json");
    assert(JSON.parse(readFileSync(file, "utf8")).mcpServers?.br8n, "disconnected before the confirmation");
    await clickButton(page, "Disconnect", "data-agent='cursor'");
    await waitForState(page, "cursor", "not_connected");
    const text = (await card(page, "cursor")).text;
    assert(text.includes("Disconnected Cursor."), `no disconnect report:\n${text}`);
    assert(!JSON.parse(readFileSync(file, "utf8")).mcpServers?.br8n, "br8n's entry is still in mcp.json");
  },

  "an unparseable gemini config is refused and left byte-identical": async ({ page, sandbox }) => {
    const dir = join(sandbox.home, ".gemini");
    mkdirSync(dir, { recursive: true });
    const file = join(dir, "settings.json");
    const broken = '{\n  // a comment JSON does not allow\n  "theme": "dark",\n}\n';
    writeFileSync(file, broken);
    await reloadPage(page);
    await openIntegrations(page);
    await waitForState(page, "gemini", "broken");
    assert((await card(page, "gemini")).text.includes("cannot be read as JSON"), "the card does not say why gemini is broken");
    await clickButton(page, "Repair", "data-agent='gemini'");
    await page.waitForSelector("[data-agent=gemini] [data-testid=agent-error]", { timeout: 20_000 });
    const text = (await card(page, "gemini")).text;
    assert(text.includes("Nothing was written"), `the refusal does not say the file was left alone:\n${text}`);
    assert(readFileSync(file, "utf8") === broken, "settings.json changed");
  },

  "codex offers AGENTS.md guidance, off by default": async ({ page, sandbox }) => {
    sandbox.pretendInstalled(".codex");
    await reloadPage(page);
    await openIntegrations(page);
    const box = await page.waitForSelector("[data-agent=codex] input[type=checkbox]");
    assert((await box.evaluate((el) => el.checked)) === false, "the AGENTS.md checkbox starts checked");
    const text = (await card(page, "codex")).text;
    assert(text.includes("read in every repository"), `no explanation of the global file:\n${text}`);
    assert(text.includes("index_transcripts = false"), `no sessions line:\n${text}`);
  },

  "any other MCP client gets copyable snippets": async ({ page }) => {
    await openIntegrations(page);
    const text = await page.$eval("[data-testid=other-mcp]", (el) => el.innerText);
    assert(text.includes('"mcpServers"') && text.includes("[mcp_servers.br8n]"), `snippets missing:\n${text}`);
    const copies = await page.$$("[data-testid=other-mcp] button");
    assert(copies.length === 2, `expected two Copy buttons, found ${copies.length}`);
  },
};

export const allowErrors = [/status of 409 \(Conflict\)/];
