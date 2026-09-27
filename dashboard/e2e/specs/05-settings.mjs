import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { assert, openTab, waitForText } from "../helpers.mjs";

async function api(page, path, body) {
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

async function clickText(page, selector, text) {
  const handle = await page.waitForFunction(
    (selector, text) => [...document.querySelectorAll(selector)].find((el) => el.innerText.trim().startsWith(text) && !el.disabled),
    { timeout: 10_000 },
    selector,
    text,
  );
  await handle.asElement().click();
}

async function openSettings(page, section) {
  await openTab(page, "Settings");
  await page.waitForSelector("[data-testid=settings]");
  await clickText(page, "[data-testid=settings] nav button", section);
  await page.waitForFunction((s) => document.querySelector("[data-testid=settings] h2")?.innerText === s, {}, section);
}

async function fill(page, selector, value) {
  const input = await page.waitForSelector(selector);
  await input.click({ clickCount: 3 });
  await page.keyboard.press("Backspace");
  await input.type(value);
}

async function saveThroughBar(page) {
  await page.waitForFunction(() => {
    const bar = document.querySelector("[data-testid=save-bar]");
    return bar && !bar.innerText.includes("checking");
  });
  await clickText(page, "[data-testid=save-bar] button", "Save");
  await page.waitForSelector("[data-testid=diff-preview]");
  await clickText(page, "[data-testid=save-bar] button", "Write config.toml");
}

export const allowErrors = [/status of 409 \(Conflict\)/];

export const tests = {
  "retrieval changes are previewed, saved and survive a reload": async ({ page, sandbox }) => {
    sandbox.writeConfig();
    await openSettings(page, "Retrieval");
    await fill(page, 'input[aria-label="hook.threshold"]', "0.5");
    const radios = await page.$$("button[role=radio]");
    const labels = await Promise.all(radios.map((r) => r.evaluate((el) => el.innerText.trim())));
    await radios[labels.findIndex((l) => l.startsWith("balanced"))].click();
    await waitForText(page, "2 unsaved changes");
    await page.waitForFunction(() => !document.querySelector("[data-testid=save-bar]").innerText.includes("checking"));
    await clickText(page, "[data-testid=save-bar] button", "Save");
    const preview = await page.$eval("[data-testid=diff-preview]", (el) => el.innerText);
    assert(/hook\.threshold[\s\S]*default 0\.66[\s\S]*0\.5/.test(preview), `the preview shows old and new:\n${preview}`);
    assert(preview.includes("hook.quality"), `the preview lists the tier:\n${preview}`);
    await clickText(page, "[data-testid=save-bar] button", "Write config.toml");
    await page.waitForSelector("[data-testid=settings-notice]");
    await waitForText(page, "nothing needs a restart");
    assert(!(await page.$("[data-testid=save-bar]")), "the save bar goes away after a save");

    const { json } = await api(page, "/api/config");
    assert(json.effective.hook.threshold === 0.5, `threshold on disk: ${json.effective.hook.threshold}`);
    assert(json.effective.hook.quality === 2, `quality on disk: ${json.effective.hook.quality}`);

    await page.reload({ waitUntil: "networkidle0" });
    await openSettings(page, "Retrieval");
    const value = await page.$eval('input[aria-label="hook.threshold"]', (el) => el.value);
    assert(value === "0.5", `the reloaded form shows the saved threshold, got ${value}`);
    const checked = await page.$eval("button[role=radio][aria-checked=true]", (el) => el.innerText);
    assert(checked.startsWith("balanced"), `the reloaded form shows the saved tier, got ${checked}`);
  },

  "an out-of-range value is flagged inline and cannot be saved": async ({ page, sandbox }) => {
    sandbox.writeConfig();
    const before = readFileSync(sandbox.configPath, "utf8");
    await openSettings(page, "Retrieval");
    await fill(page, 'input[aria-label="hook.threshold"]', "3");
    await waitForText(page, "must be between 0 and 1");
    const disabled = await page.$$eval("[data-testid=save-bar] button", (bs) =>
      bs.filter((b) => b.innerText.trim() === "Save").every((b) => b.disabled),
    );
    assert(disabled, "Save is disabled while a value is out of range");
    assert(readFileSync(sandbox.configPath, "utf8") === before, "nothing was written");
  },

  "a source that does not exist is refused before it is added": async ({ page, sandbox }) => {
    sandbox.writeConfig();
    await openSettings(page, "Sources");
    await fill(page, 'input[aria-label="add to sources"]', join(sandbox.root, "no-such-folder"));
    const error = await page.waitForSelector("[data-testid=sources-candidate-error]");
    const text = await error.evaluate((el) => el.innerText);
    assert(text.includes("does not exist"), `inline error: ${text}`);
    const addDisabled = await page.$$eval("button", (bs) => bs.find((b) => b.innerText.trim() === "Add folder")?.disabled);
    assert(addDisabled === true, "Add is disabled for a missing folder");
  },

  "a new folder is added, re-indexed and counted": async ({ page, sandbox }) => {
    sandbox.writeConfig();
    const extra = join(sandbox.root, "extra-notes");
    mkdirSync(extra, { recursive: true });
    writeFileSync(join(extra, "kombucha.md"), "# Kombucha\n\nThe second fermentation runs three days with ginger.\n");
    await openSettings(page, "Sources");
    await fill(page, 'input[aria-label="add to sources"]', extra);
    await page.waitForFunction(
      () => [...document.querySelectorAll("button")].some((b) => b.innerText.trim() === "Add folder" && !b.disabled),
      { timeout: 10_000 },
    );
    await clickText(page, "button", "Add folder");
    await saveThroughBar(page);
    await page.waitForSelector("[data-testid=settings-notice]");
    const { json } = await api(page, "/api/config");
    assert(json.effective.sources.includes(extra), `the new source is saved: ${JSON.stringify(json.effective.sources)}`);

    await clickText(page, "button", "Re-index now");
    await page.waitForFunction(
      () => document.querySelector("[data-testid=index-progress]") || document.body.innerText.includes("Index finished"),
      { timeout: 30_000 },
    );
    await waitForText(page, "Index finished", 120_000);
    await waitForText(page, `${sandbox.corpusDocuments() + 1} docs`, 10_000);
  },

  "a hand edit made while the page is open is a conflict, and keep editing saves on top": async ({ page, sandbox }) => {
    sandbox.writeConfig();
    await openSettings(page, "Retrieval");
    const inputs = await page.$$('input[aria-label="hook.max_tokens"]');
    assert(inputs.length === 1, "one hook max_tokens field");
    await fill(page, 'input[aria-label="hook.max_tokens"]', "900");
    sandbox.writeConfig("\n[mcp]\nmax_tokens = 1234\n");
    await saveThroughBar(page);
    await page.waitForSelector("[data-testid=conflict]");
    const onDisk = readFileSync(sandbox.configPath, "utf8");
    assert(!onDisk.includes("900"), `the hand edit was not overwritten:\n${onDisk}`);
    await clickText(page, "[data-testid=conflict] button", "Keep editing");
    await page.waitForFunction(() => !document.querySelector("[data-testid=conflict]"));
    await saveThroughBar(page);
    await page.waitForSelector("[data-testid=settings-notice]");
    const merged = readFileSync(sandbox.configPath, "utf8");
    assert(merged.includes("max_tokens = 1234") && merged.includes("max_tokens = 900"), `both edits survive:\n${merged}`);
  },

  "test connection reports the stub's dimensions": async ({ page, sandbox }) => {
    sandbox.writeConfig();
    await openSettings(page, "Embedding");
    await clickText(page, "button", "Test connection");
    const result = await page.waitForSelector("[data-testid=embed-test-result]", { timeout: 20_000 });
    const text = await result.evaluate((el) => el.innerText);
    const { json } = await api(page, "/api/config");
    const expected = `${json.effective.embed.dimensions} dimensions`;
    assert(/Connected to Ollama in \d+ ms/.test(text) && text.includes(expected), `test result: ${text}, expected ${expected}`);
  },

  "changing the model warns that a full re-index is needed": async ({ page, sandbox }) => {
    sandbox.writeConfig();
    await openSettings(page, "Embedding");
    await fill(page, 'input[aria-label="embed.model"]', "nomic-embed-text");
    await page.waitForSelector("[data-testid=reindex-warning]");
    await waitForText(page, "br8n index --reindex");
    await clickText(page, "[data-testid=save-bar] button", "Discard");
    await page.waitForFunction(() => !document.querySelector("[data-testid=reindex-warning]"));
  },

  "the embedding token is write-only": async ({ page, sandbox }) => {
    sandbox.writeConfig();
    const secret = "sk-e2e-never-shown";
    try {
      await openSettings(page, "Embedding");
      await clickText(page, "button[role=radio]", "Remote endpoint");
      await fill(page, 'input[aria-label="endpoint.url"]', `${sandbox.ollama.url}/v1`);
      await fill(page, 'input[aria-label="endpoint.model"]', "stub-model");
      await clickText(page, "button", "Set token");
      await fill(page, 'input[aria-label="embed.token"]', secret);
      await clickText(page, "button", "Save endpoint");
      await page.waitForFunction(() => document.querySelector("[data-testid=token-state]")?.innerText === "set");
      assert(!(await page.$('input[aria-label="embed.token"]')), "the token field closes after saving");
      const html = await page.content();
      assert(!html.includes(secret), "the page never holds the token after saving");
      const { json } = await api(page, "/api/config");
      assert(!JSON.stringify(json).includes(secret), "GET /api/config never returns the token");
      assert(json.secrets["embed.token"] === "set", "the token is reported as set");

      await page.reload({ waitUntil: "networkidle0" });
      await openSettings(page, "Embedding");
      await page.waitForFunction(() => document.querySelector("[data-testid=token-state]")?.innerText === "set");
      assert(!(await page.content()).includes(secret), "a reload does not bring the token back");
    } finally {
      await api(page, "/api/config/embed", { url: null, model: null, token: null });
    }
  },

  "the raw TOML view turns an edit into unsaved changes and keeps comments": async ({ page, sandbox }) => {
    sandbox.writeConfig("# keep this comment\n");
    await openSettings(page, "Advanced");
    const raw = await page.waitForSelector('textarea[aria-label="raw config.toml"]');
    const shown = await raw.evaluate((el) => el.value);
    assert(shown.includes("ollama_url") && !shown.includes("keep this comment"), `the raw view mirrors the file's keys:\n${shown}`);
    await raw.click();
    await page.keyboard.down("Control");
    await page.keyboard.press("End");
    await page.keyboard.up("Control");
    await page.keyboard.type("\n[memory]\nmax_memories = 777\n");
    await clickText(page, "button", "Apply to form");
    await waitForText(page, "1 unsaved change");
    await saveThroughBar(page);
    await page.waitForSelector("[data-testid=settings-notice]");
    const written = readFileSync(sandbox.configPath, "utf8");
    assert(written.includes("max_memories = 777"), `the raw edit is on disk:\n${written}`);
    assert(written.includes("# keep this comment"), `the comment survives:\n${written}`);
  },
};
