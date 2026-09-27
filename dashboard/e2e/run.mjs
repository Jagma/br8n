import { readdirSync, mkdirSync } from "node:fs";
import { join, resolve } from "node:path";
import puppeteer from "puppeteer";
import { Sandbox } from "./br8n.mjs";

const HERE = new URL(".", import.meta.url).pathname;
const SHOTS = resolve(HERE, "../../target/e2e-shots");
const only = process.argv[2];

async function launch() {
  const options = { headless: "shell", args: ["--no-sandbox"] };
  if (process.env.BR8N_E2E_CHROME) options.executablePath = process.env.BR8N_E2E_CHROME;
  return puppeteer.launch(options);
}

async function newPage(browser, errors) {
  const page = await browser.newPage();
  await page.setViewport({ width: 1400, height: 900 });
  page.on("console", (m) => m.type() === "error" && errors.push(`console: ${m.text()}`));
  page.on("pageerror", (e) => errors.push(`pageerror: ${e.message}`));
  return page;
}

async function main() {
  mkdirSync(SHOTS, { recursive: true });
  const specs = readdirSync(join(HERE, "specs"))
    .filter((f) => f.endsWith(".mjs"))
    .filter((f) => !only || f.includes(only))
    .sort();
  const sandbox = await Sandbox.create();
  const browser = await launch();
  const results = [];
  const fresh = [];
  try {
    await sandbox.run("index");
    await sandbox.startDashboard();
    for (const file of specs) {
      const spec = await import(join(HERE, "specs", file));
      let target = sandbox;
      if (spec.fresh) {
        target = await Sandbox.create({ indexed: false });
        fresh.push(target);
        await target.startDashboard();
      }
      for (const [name, test] of Object.entries(spec.tests)) {
        const errors = [];
        const page = await newPage(browser, errors);
        const label = `${file.replace(/\.mjs$/, "")} › ${name}`;
        const shot = join(SHOTS, `${label.replace(/[^a-z0-9]+/gi, "-")}.png`);
        const snap = (step) => page.screenshot({ path: join(SHOTS, `${file.replace(/\.mjs$/, "")}-${step}.png`), fullPage: true });
        try {
          await page.goto(target.url, { waitUntil: "networkidle0" });
          await test({ page, sandbox: target, errors, snap });
          const unexpected = errors.filter((e) => !(spec.allowErrors ?? []).some((re) => re.test(e)));
          if (unexpected.length) throw new Error(`browser errors:\n  ${unexpected.join("\n  ")}`);
          results.push({ label, ok: true });
        } catch (e) {
          results.push({ label, ok: false, error: e.message });
        } finally {
          await page.screenshot({ path: shot, fullPage: true }).catch(() => {});
          await page.close();
        }
      }
    }
  } finally {
    await browser.close();
    sandbox.close();
    for (const s of fresh) s.close();
  }
  for (const r of results) console.log(`${r.ok ? "ok  " : "FAIL"} ${r.label}${r.ok ? "" : `\n     ${r.error}`}`);
  const failed = results.filter((r) => !r.ok).length;
  console.log(`\n${results.length - failed} passed, ${failed} failed; screenshots in ${SHOTS}`);
  process.exit(failed ? 1 : 0);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
