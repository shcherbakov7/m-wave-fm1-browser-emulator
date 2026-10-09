// Boot every firmware in web/firmware/catalog.json in the native emulator and
// record in the catalog whether it ran without stopping and drew a screen, so
// the page can tell which builds are known to work.
// Needs: cargo build --release --manifest-path emulator/Cargo.toml --example diagnose
// Usage: node tools/check-firmware.mjs [instructions]
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve(new URL("..", import.meta.url).pathname);
const diagnose = resolve(root, "emulator/target/release/examples/diagnose");
const catalogPath = resolve(root, "web/firmware/catalog.json");
const limit = process.argv[2] ?? "600000000";

const catalog = JSON.parse(readFileSync(catalogPath, "utf8"));
for (const entry of catalog) {
  if (!entry.file) continue;
  const run = spawnSync(diagnose, [resolve(root, "web", entry.file), limit], {
    env: { ...process.env, DIAG_FAST: "1" }, encoding: "utf8", timeout: 300_000,
  });
  const output = `${run.stdout}\n${run.stderr}`;
  const lcd = Number(output.match(/LCD: (\d+) pixels/)?.[1] ?? 0);
  const stop = output.match(/^diagnose: (.*)$/m)?.[1] ?? (run.error ? String(run.error) : "no result");
  const ran = stop.startsWith("instruction limit");
  entry.check = { ok: ran && lcd > 0, lcd, stop: ran ? "" : stop.slice(0, 200) };
  console.log(`${entry.name} ${entry.version}: ${entry.check.ok ? "ok" : "PROBLEM"} (LCD ${lcd} pixels${ran ? "" : `; ${stop}`})`);
}
writeFileSync(catalogPath, `${JSON.stringify(catalog, null, 2)}\n`);
