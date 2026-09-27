import { readFileSync } from "node:fs";
import { assert, clickButton, reloadPage, waitForText } from "../helpers.mjs";

export const fresh = true;

async function setupShown(page) {
  await page.waitForSelector("[data-testid=onboarding]", { timeout: 20_000 });
}

async function setupGone(page) {
  await page.waitForFunction(
    () => !document.querySelector("[data-testid=onboarding]") && !document.body.innerText.includes("Loading…"),
    { timeout: 20_000 },
  );
}

export const tests = {
  "skip setup returns to the tabs, stays skipped, and the graph can reopen it": async ({ page }) => {
    await setupShown(page);
    await clickButton(page, "Skip setup");
    await setupGone(page);
    await waitForText(page, "No documents indexed yet");
    await reloadPage(page);
    await setupGone(page);
    await waitForText(page, "No documents indexed yet");
    await clickButton(page, "Open guided setup");
    await setupShown(page);
  },

  "setup picks a folder, connects cursor, indexes and lands on search": async ({ page, sandbox, snap }) => {
    await page.evaluate(() => window.localStorage.clear());
    sandbox.pretendInstalled(".cursor");
    sandbox.placeBinary();
    await reloadPage(page);
    await setupShown(page);
    await waitForText(page, "Pick folders");
    const input = await page.waitForSelector("input[aria-label='folder to index']");
    await input.type(sandbox.corpus);
    await clickButton(page, "Add folder");
    await page.waitForSelector(`[data-testid=setup-folders] [data-item='${sandbox.corpus}']`);
    await snap("1-folders");
    await clickButton(page, "Save and continue");
    await page.waitForSelector("[data-agent=cursor]", { timeout: 30_000 });
    const config = readFileSync(sandbox.configPath, "utf8");
    assert(config.includes(sandbox.corpus), `the folder was not saved:\n${config}`);

    await clickButton(page, "Connect", "data-agent='cursor'");
    await page.waitForFunction(
      () => document.querySelector("[data-agent=cursor] [data-testid=agent-status]")?.getAttribute("data-state") === "connected",
      { timeout: 20_000 },
    );
    await snap("2-agents");
    await clickButton(page, "Continue");
    await waitForText(page, "br8n reads every file");
    await snap("3-index");

    await page.waitForSelector("[data-testid=suggested-query]", { timeout: 120_000 });
    await waitForText(page, "INJECTED", 30_000);
    const query = await page.$eval("input[placeholder^='Search']", (el) => el.value);
    assert(query.trim().length > 0, "no suggested query in the search box");
    await snap("4-search");

    await reloadPage(page);
    await setupGone(page);
    await waitForText(page, `${sandbox.corpusDocuments()} docs`);
  },
};
