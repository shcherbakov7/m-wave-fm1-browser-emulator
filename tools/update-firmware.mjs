// Fetch the newest builds of the open source FM-1 firmwares from their
// authors' repositories into web/firmware/ and write web/firmware/catalog.json,
// which the page offers as ready-to-run firmware. CI runs this before
// packaging the site, so the site follows new releases.
//
// Only firmwares whose authors publish the .fwsc in their repository under a
// free licence are copied. The official M-VAVE firmware is not copied: the
// page loads it from M-VAVE's own server.
//
// Usage: node tools/update-firmware.mjs [out-dir]
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const out = resolve(process.argv[2] ?? new URL("../web/firmware", import.meta.url).pathname);

const SOURCES = [
  {
    id: "felucca", name: "Felucca", author: "Leo Kuroshita (Hügelton Instruments)",
    repo: "hugelton/Felucca", branch: "gh-pages", pattern: /^firmware\/felucca-([\d.]+)\.fwsc$/,
    description: "Многодвижковый синтезатор: 13 движков, четыре дорожки, секвенсор, песни.",
  },
  {
    id: "sloop", name: "SLOOP", author: "3dSam",
    repo: "isod89/sloop-fm1", branch: "main", pattern: /^docs\/firmware\/sloop-([\d.]+)\.fwsc$/,
    description: "Четырёхдорожечный грувбокс: три синтезатора и драм-машина.",
  },
  {
    id: "x0x", name: "X0X", author: "Charles Vestal",
    repo: "charlesvestal/fm1-x0x", branch: "gh-pages", pattern: /^firmware\/x0x-([\w.-]+)\.fwsc$/,
    description: "Грувбокс в духе ReBirth: TR-909, TR-808 и два TB-303.",
  },
  {
    id: "melodee", name: "Melodee", author: "keremimo",
    repo: "keremimo/melodee", branch: "gh-pages", pattern: /^firmware\/melodee-([\d.]+)\.fwsc$/,
    description: "Десять движков, восемь паттернов на дорожку, рабочие пространства STUDIO.",
  },
  {
    id: "sloop-alg", name: "SLOOP ALG", author: "shaw-core",
    repo: "shaw-core/Sloop_ALG02", branch: "main", pattern: /^firmware\/sloop-(ALG\d+)-TEST\.fwsc$/,
    description: "Экспериментальная ветка SLOOP с движками DX7, VA, Karplus-Strong.",
  },
];

const OFFICIAL = {
  id: "official", name: "Официальная M-VAVE", version: "V15", author: "M-VAVE",
  url: "https://yms-file-store.oss-cn-hongkong.aliyuncs.com/software/firmware/FM-1.fwsc",
  page: "https://www.m-vave.com/download",
  license: "проприетарная, загружается с сервера M-VAVE",
  description: "Заводская прошивка: 6-операторный FM в духе DX7, эффекты, арпеджиатор, секвенсор.",
};

/** Compare dotted versions numerically, part by part. */
function compareVersions(a, b) {
  const pa = a.split(/[.-]/), pb = b.split(/[.-]/);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const x = pa[i] ?? "", y = pb[i] ?? "";
    const nx = Number.parseInt(x.replace(/\D+/g, ""), 10), ny = Number.parseInt(y.replace(/\D+/g, ""), 10);
    if (!Number.isNaN(nx) && !Number.isNaN(ny) && nx !== ny) return nx - ny;
    if (x !== y) return x < y ? -1 : 1;
  }
  return 0;
}

const git = (dir, ...args) => execFileSync("git", ["-C", dir, ...args], { maxBuffer: 64 << 20 });

mkdirSync(out, { recursive: true });
const catalog = [OFFICIAL];
for (const source of SOURCES) {
  const dir = mkdtempSync(join(tmpdir(), "fm1-fw-"));
  try {
    git(dir, "init", "-q");
    git(dir, "fetch", "-q", "--depth", "1", `https://github.com/${source.repo}`, source.branch);
    const files = git(dir, "ls-tree", "-r", "--name-only", "FETCH_HEAD").toString().split("\n");
    const builds = files
      .map((path) => ({ path, version: path.match(source.pattern)?.[1] }))
      .filter((build) => build.version)
      .sort((a, b) => compareVersions(a.version, b.version));
    const newest = builds.at(-1);
    if (!newest) throw new Error("no firmware file found");
    const bytes = git(dir, "show", `FETCH_HEAD:${newest.path}`);
    const date = git(dir, "log", "-1", "--format=%cs", "FETCH_HEAD").toString().trim();
    const file = `${source.id}-${newest.version}.fwsc`;
    writeFileSync(join(out, file), bytes);
    catalog.push({
      id: source.id, name: source.name, version: newest.version, author: source.author,
      description: source.description, file: `firmware/${file}`, size: bytes.length, date,
      license: "GPL-3.0", source: `https://github.com/${source.repo}`,
    });
    console.log(`${source.name} ${newest.version} (${bytes.length} bytes, ${date})`);
  } catch (error) {
    // One unreachable source must not take the site down.
    console.warn(`${source.name}: skipped (${error.message.split("\n")[0]})`);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}
writeFileSync(join(out, "catalog.json"), `${JSON.stringify(catalog, null, 2)}\n`);
console.log(`${catalog.length} entries in ${join(out, "catalog.json")}`);
