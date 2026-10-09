// Load a firmware in the web app with headless Chromium and screenshot it.
// Usage: node tools/browser-smoke.mjs firmware.fwsc out.png [timeoutSeconds] [press-after-boot-id ...]
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, resolve } from "node:path";
import { chromium } from "playwright";

const [firmware, screenshot, timeoutArg = "180", ...presses] = process.argv.slice(2);
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
const url = `http://127.0.0.1:${server.address().port}/`;

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
const errors = [];
page.on("pageerror", (error) => errors.push(String(error)));
page.on("console", (message) => message.type() === "error" && errors.push(message.text()));
await page.goto(url);
await page.waitForFunction(() => document.getElementById("state").dataset.key === "state.choose");
await page.setInputFiles("#file", firmware);

// Wait until the guest has drawn to its screen (or faulted).
const deadline = Date.now() + Number(timeoutArg) * 1000;
let state = "";
while (Date.now() < deadline) {
  await page.waitForTimeout(2000);
  state = await page.evaluate(() => {
    const lit = (() => {
      const data = document.getElementById("lcd").getContext("2d").getImageData(0, 0, 240, 240).data;
      let count = 0;
      for (let i = 0; i < data.length; i += 4) if (data[i] | data[i + 1] | data[i + 2]) count++;
      return count;
    })();
    return JSON.stringify({ state: document.getElementById("state").textContent, fault: document.getElementById("state").classList.contains("fault"), steps: document.getElementById("steps").textContent, speed: document.getElementById("speed").textContent, lit });
  });
  const parsed = JSON.parse(state);
  if (parsed.fault || parsed.lit > 2000) break;
}
// SETTLE=seconds: keep running after boot, then report the speed again.
if (process.env.SETTLE) {
  await page.waitForTimeout(Number(process.env.SETTLE) * 1000);
  state = await page.evaluate(() => JSON.stringify({ steps: document.getElementById("steps").textContent, speed: document.getElementById("speed").textContent, load: document.getElementById("speed").title, guest: document.getElementById("guest-time").textContent }));
}
for (const id of presses) {
  await page.locator(`.ctl[data-id="${id}"]`).click();
  await page.waitForTimeout(8000);
}
await page.screenshot({ path: screenshot });
console.log(state);
if (errors.length) console.log("page errors:", errors);
await browser.close();
server.close();
