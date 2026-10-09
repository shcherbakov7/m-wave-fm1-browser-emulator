// Drive the web panel in headless Chromium: boot a firmware, then turn each
// knob with the wheel and by dragging, press buttons and glide over the
// keys, reporting how the guest screen and LEDs respond.
// Usage: node tools/browser-panel.mjs firmware.fwsc [screenshot.png]
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, resolve } from "node:path";
import { chromium } from "playwright";

const [firmware, screenshot] = process.argv.slice(2);
const root = resolve(new URL("../web", import.meta.url).pathname);
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".wasm": "application/wasm" };
const server = createServer(async (request, response) => {
  const path = join(root, decodeURIComponent(new URL(request.url, "http://x").pathname).replace(/\/$/, "/index.html"));
  try {
    response.writeHead(200, { "content-type": types[extname(path)] ?? "application/octet-stream" });
    response.end(await readFile(path));
  } catch {
    response.writeHead(404).end();
  }
}).listen(0);

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
const errors = [];
page.on("pageerror", (error) => errors.push(String(error)));
page.on("console", (message) => message.type() === "error" && errors.push(message.text()));
await page.goto(`http://127.0.0.1:${server.address().port}/`);
await page.waitForFunction(() => document.getElementById("state").textContent.startsWith("Загрузите"));
await page.setInputFiles("#file", firmware);

const lcd = () => page.evaluate(() => Array.from(document.getElementById("lcd").getContext("2d").getImageData(0, 0, 240, 240).data));
const differs = (a, b) => { let n = 0; for (let i = 0; i < a.length; i += 4) if (a[i] !== b[i] || a[i + 1] !== b[i + 1] || a[i + 2] !== b[i + 2]) n++; return n; };
const guestWait = async (seconds) => {
  // Wait for guest time, however fast the emulator runs.
  const read = () => page.evaluate(() => parseFloat(document.getElementById("guest-time").textContent) || 0);
  const target = (await read()) + seconds;
  while ((await read()) < target) await page.waitForTimeout(200);
};
const lit = () => page.evaluate(() => [...document.querySelectorAll(".ctl")]
  .filter((e) => parseFloat(getComputedStyle(e).getPropertyValue("--led")) > 0.5)
  .map((e) => e.getAttribute("aria-label")).join(", "));

// Boot until the screen shows something.
for (let i = 0; i < 90 && (await lcd()).filter((v, j) => j % 4 === 0 && v).length < 2000; i++) await page.waitForTimeout(1000);
await guestWait(1.5);
console.log("lit LEDs:", (await lit()) || "none");

const knobs = await page.$$(".knob");
const names = await Promise.all(knobs.map((k) => k.getAttribute("aria-label")));
for (const [i, knob] of knobs.entries()) {
  const box = await knob.boundingBox();
  const before = await lcd();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  for (let n = 0; n < 3; n++) await page.mouse.wheel(0, -100);
  await guestWait(0.5);
  const wheel = differs(before, await lcd());
  const mid = await lcd();
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2 + 40, { steps: 8 });
  await page.mouse.up();
  await guestWait(0.5);
  console.log(`${names[i]}: wheel changed ${wheel} px, drag back changed ${differs(mid, await lcd())} px`);
}
for (const label of ["FX", "HOME"]) {
  const before = await lcd();
  await page.click(`.ctl[aria-label="${label}"]`);
  await guestWait(0.6);
  console.log(`${label}: screen changed ${differs(before, await lcd())} px; lit: ${(await lit()) || "none"}`);
}
// Glide over the lower row of pads with one pointer.
const pads = await page.$$(".pad:not(.upper)");
const first = await pads[0].boundingBox(), last = await pads[6].boundingBox();
await page.mouse.move(first.x + first.width / 2, first.y + first.height / 2);
await page.mouse.down();
console.log("glide starts on:", await page.evaluate(([x, y]) => document.elementFromPoint(x, y)?.className, [first.x + first.width / 2, first.y + first.height / 2]),
  "held:", await page.evaluate(() => document.querySelectorAll(".pad.down").length));
const pressedDuring = [];
for (let s = 1; s <= 12; s++) {
  await page.mouse.move(first.x + first.width / 2 + ((last.x - first.x) * s) / 12, first.y + first.height / 2);
  pressedDuring.push(await page.evaluate(() => document.querySelectorAll(".pad.down").length));
}
await page.mouse.up();
console.log("pads held while gliding:", pressedDuring.join(""), "after release:", await page.evaluate(() => document.querySelectorAll(".down").length));
if (screenshot) await page.screenshot({ path: screenshot });
if (errors.length) console.log("page errors:", errors);
await browser.close();
server.close();
