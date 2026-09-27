export async function openTab(page, name) {
  const [tab] = await page.$$(`xpath/.//header//button[normalize-space()='${name}'] | .//nav//button[normalize-space()='${name}'] | .//button[normalize-space()='${name}']`);
  if (!tab) throw new Error(`no tab named ${name}`);
  await tab.click();
}

export async function waitForText(page, text, timeout = 10_000) {
  await page.waitForFunction((t) => document.body.innerText.includes(t), { timeout }, text);
}

export function assert(condition, message) {
  if (!condition) throw new Error(message);
}

export async function clickButton(page, text, within = "") {
  const scope = within ? `//*[@${within}]` : "";
  const handle = await page.waitForSelector(
    `xpath/${scope}//button[normalize-space()='${text}' and not(@disabled)]`,
    { timeout: 15_000 },
  );
  await handle.click();
}

export async function reloadPage(page) {
  await page.reload({ waitUntil: "networkidle0" });
}
