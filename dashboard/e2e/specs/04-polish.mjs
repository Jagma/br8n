import { assert, openTab, waitForText } from "../helpers.mjs";

const NOTE_RGB = [0x2e, 0x9e, 0x78];
const TITLES = ["Deploy checklist", "Garden notes", "Sourdough starter", "Ingest service runbook"];

async function search(page, query) {
  await openTab(page, "Search");
  const input = await page.waitForSelector("input");
  await input.type(query);
  await page.keyboard.press("Enter");
  await waitForText(page, "INJECTED");
}

function columnText(page, name) {
  return page.evaluate((n) => {
    const label = [...document.querySelectorAll("span")].find((s) => s.textContent === n);
    return label ? label.parentElement.parentElement.innerText : null;
  }, name);
}

async function noteCentre(page) {
  return page.evaluate((rgb) => {
    const canvas = document.querySelector("canvas");
    const { data, width, height } = canvas.getContext("2d").getImageData(0, 0, canvas.width, canvas.height);
    const hits = [];
    for (let y = 0; y < height; y += 2) {
      for (let x = 0; x < width; x += 2) {
        const i = (y * width + x) * 4;
        if (data[i] === rgb[0] && data[i + 1] === rgb[1] && data[i + 2] === rgb[2]) hits.push([x, y]);
      }
    }
    if (!hits.length) return null;
    const [sx, sy] = hits[0];
    const near = hits.filter(([x, y]) => Math.hypot(x - sx, y - sy) < 40);
    const cx = near.reduce((a, [x]) => a + x, 0) / near.length;
    const cy = near.reduce((a, [, y]) => a + y, 0) / near.length;
    const rect = canvas.getBoundingClientRect();
    const scale = rect.width / canvas.width;
    return { x: rect.left + cx * scale, y: rect.top + cy * scale };
  }, NOTE_RGB);
}

async function settledNoteCentre(page) {
  let last = null;
  for (let i = 0; i < 60; i++) {
    const now = await noteCentre(page);
    if (now && last && Math.hypot(now.x - last.x, now.y - last.y) < 1) return now;
    last = now;
    await new Promise((r) => setTimeout(r, 250));
  }
  throw new Error(`the graph never settled; last note centre ${JSON.stringify(last)}`);
}

export const tests = {
  "a bm25 card shows its own score, not an empty relevance": async ({ page }) => {
    await search(page, "how often do I feed the sourdough starter");
    const text = await columnText(page, "bm25");
    assert(text !== null, "no bm25 column");
    assert(/bm25 \d+\.\d\d/.test(text), `expected a native bm25 score in the column, got:\n${text}`);
    assert(!text.includes("0.000"), `a bm25 card still renders an unmeasured relevance of 0.000:\n${text}`);
  },

  "every card says which part of its document matched": async ({ page }) => {
    await search(page, "roll back the ingest service when the queue backs up");
    const subtitles = await page.evaluate(() =>
      [...document.querySelectorAll("button[aria-expanded]")].map((b) => ({
        title: b.innerText.replace(/[▸▾]/g, "").trim(),
        subtitle: b.nextElementSibling?.classList.contains("truncate") ? b.nextElementSibling.innerText.trim() : "",
      })),
    );
    assert(subtitles.length > 0, "no result cards");
    const missing = subtitles.filter((s) => !s.subtitle);
    assert(missing.length === 0, `cards without a second line: ${JSON.stringify(missing)}`);
    const runbook = subtitles.filter((s) => s.title.startsWith("Ingest service runbook")).map((s) => s.subtitle);
    assert(runbook.includes("Rollback"), `expected a runbook card headed Rollback, got ${JSON.stringify(runbook)}`);
    assert(
      runbook.some((s) => s.startsWith("The ingest service reads uploads")),
      `expected the runbook's opening chunk to show its first line, got ${JSON.stringify(runbook)}`,
    );
    assert(new Set(runbook).size > 1, `the runbook's chunks are indistinguishable: ${JSON.stringify(runbook)}`);
  },

  "the latency card says how to populate it": async ({ page }) => {
    await openTab(page, "Health");
    await waitForText(page, "Latency p50");
    const text = await page.evaluate(() => {
      const label = [...document.querySelectorAll("span")].find((s) => s.textContent === "Latency p50");
      return label.parentElement.innerText;
    });
    assert(text.includes("br8n bench"), `latency card has no empty state:\n${text}`);
  },

  "hovering a graph node names it": async ({ page }) => {
    await openTab(page, "Graph");
    await page.waitForSelector("canvas", { timeout: 10_000 });
    const at = await settledNoteCentre(page);
    await page.mouse.move(at.x, at.y);
    const label = await page.waitForFunction(
      (titles) => {
        const tip = document.querySelector(".float-tooltip-kap");
        if (!tip || getComputedStyle(tip).visibility === "hidden" || getComputedStyle(tip).display === "none") return null;
        return titles.find((t) => tip.innerText.includes(t)) ?? null;
      },
      { timeout: 5_000 },
      TITLES,
    );
    assert(await label.jsonValue(), "no node title on hover");
  },
};
