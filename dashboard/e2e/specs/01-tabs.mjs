import { assert, clickButton, openTab, reloadPage, waitForText } from "../helpers.mjs";

export const tests = {
  "header counts the indexed corpus": async ({ page, sandbox }) => {
    await waitForText(page, `${sandbox.corpusDocuments()} docs`);
  },

  "graph renders a canvas": async ({ page }) => {
    await openTab(page, "Graph");
    await page.waitForSelector("canvas", { timeout: 10_000 });
  },

  "search finds the note that answers": async ({ page }) => {
    await openTab(page, "Search");
    const input = await page.waitForSelector("input");
    await input.type("how often do I feed the sourdough starter");
    await page.keyboard.press("Enter");
    await waitForText(page, "INJECTED");
    await waitForText(page, "Sourdough starter");
  },

  "memories lists nothing and offers New": async ({ page }) => {
    await openTab(page, "Memories");
    await waitForText(page, "New");
  },

  "health shows the document tile": async ({ page, sandbox }) => {
    await openTab(page, "Health");
    await waitForText(page, "DOCUMENTS");
    const text = await page.evaluate(() => document.body.innerText);
    const expected = sandbox.corpusDocuments();
    assert(new RegExp(`DOCUMENTS\\s+${expected}\\b`).test(text), `expected ${expected} documents on Health, got:\n${text.slice(0, 400)}`);
  },

  "health lists the install problems of an uninstalled build": async ({ page }) => {
    await openTab(page, "Health");
    await page.waitForSelector("[data-testid=install-card] [data-check=binary]", { timeout: 20_000 });
    const card = await page.$eval("[data-testid=install-card]", (el) => el.innerText);
    for (const check of ["Installed binary", "Plugin files"]) {
      assert(card.includes(check), `the install card does not list ${check}:\n${card}`);
    }
    assert(/\d+ problems?/.test(card), `no problem count:\n${card}`);
    assert(card.includes(" install"), `no install command offered:\n${card}`);
    await page.waitForFunction(
      () => document.querySelector("[data-testid=embed-line]")?.innerText.includes("reachable, "),
      { timeout: 20_000 },
    );
  },

  "a config error on health opens the settings field it names": async ({ page, sandbox }) => {
    sandbox.writeConfig("\n[hook]\nthreshold = 2\n");
    try {
      await reloadPage(page);
      await openTab(page, "Health");
      await clickButton(page, "Open in Settings", "data-config-error='hook.threshold'");
      await page.waitForFunction(
        () => document.getElementById("field-hook-threshold")?.classList.contains("ring-2"),
        { timeout: 10_000 },
      );
      await waitForText(page, "Retrieval");
    } finally {
      sandbox.writeConfig();
    }
  },
};
