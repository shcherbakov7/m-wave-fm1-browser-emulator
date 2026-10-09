// Fetch the newest builds of the open source FM-1 firmwares from their
// authors' repositories (files kept in a branch, or GitHub release assets)
// into web/firmware/ and write web/firmware/catalog.json, which the page
// offers as ready-to-run firmware. CI runs this before packaging the site, so
// the site follows new releases. Release lookups use the GitHub API
// (GITHUB_TOKEN, when set, raises its rate limit).
//
// Only firmwares whose authors publish the .fwsc in their repository under a
// free licence are copied. Descriptions are given per interface language. The official M-VAVE firmware is not copied: the
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
    description: { en: "Multi-engine synthesizer: 13 engines, four tracks, sequencer, songs.", ru: "Многодвижковый синтезатор: 13 движков, четыре дорожки, секвенсор, песни." },
  },
  {
    id: "sloop", name: "SLOOP", author: "3dSam",
    repo: "isod89/sloop-fm1", branch: "main", pattern: /^docs\/firmware\/sloop-([\d.]+)\.fwsc$/,
    description: { en: "Four-track groovebox: three synths and a drum machine.", ru: "Четырёхдорожечный грувбокс: три синтезатора и драм-машина." },
  },
  {
    id: "x0x", name: "X0X", author: "Charles Vestal",
    repo: "charlesvestal/fm1-x0x", branch: "gh-pages", pattern: /^firmware\/x0x-([\w.-]+)\.fwsc$/,
    description: { en: "ReBirth-style groovebox: TR-909, TR-808 and two TB-303s.", ru: "Грувбокс в духе ReBirth: TR-909, TR-808 и два TB-303." },
  },
  {
    id: "melodee", name: "Melodee", author: "keremimo",
    repo: "keremimo/melodee", branch: "gh-pages", pattern: /^firmware\/melodee-([\d.]+)\.fwsc$/,
    description: { en: "Ten engines, eight patterns per track, STUDIO workspaces.", ru: "Десять движков, восемь паттернов на дорожку, рабочие пространства STUDIO." },
  },
  {
    id: "fomni", name: "FoMni", author: "Charles Vestal",
    repo: "charlesvestal/fm1-fomni", branch: "gh-pages", pattern: /^firmware\/omni-([\d.]+)\.fwsc$/,
    description: { en: "Omnichord: chords under one hand, a strum plate under the other.", ru: "Омникорд: аккорды под одной рукой, струнная пластина под другой." },
  },
  {
    id: "jangada", name: "Jangada", author: "zednaked",
    repo: "zednaked/jangada", release: /\.(fwsc|ufw)$/i,
    description: { en: "A Felucca fork: superwave, mod matrix, drones, ratchets, synth drums.", ru: "Ветка Felucca: superwave, матрица модуляции, дроны, рэтчеты, синтез-ударные." },
  },
  {
    id: "choralroot", name: "ChoralRoot", author: "Quixotic7",
    repo: "Quixotic7/ChoralRootFM1", release: /\.(fwsc|ufw)$/i,
    description: { en: "Chord instrument in the spirit of Telepathic Orchid: roots with one hand, chords with the other.", ru: "Аккордовый инструмент в духе Telepathic Orchid: корни одной рукой, аккорды другой." },
  },
  {
    id: "fimba", name: "FiMba", author: "jadamsowers",
    repo: "jadamsowers/fm1-fimba", release: /\.(fwsc|ufw)$/i,
    description: { en: "Kalimba (thumb piano) for the FM-1.", ru: "Калимба (пианино для больших пальцев) на FM-1." },
  },
  {
    id: "nes", name: "fm1-nes", author: "Keitark", license: "Apache-2.0",
    repo: "Keitark/fm1-nes", release: /\.(fwsc|ufw)$/i,
    description: { en: "NES emulator on the FM-1 (an example of custom firmware development).", ru: "Эмулятор NES на FM-1 (пример разработки своей прошивки)." },
  },
  {
    id: "sloop-alg", name: "SLOOP ALG", author: "shaw-core",
    repo: "shaw-core/Sloop_ALG02", branch: "main", pattern: /^firmware\/sloop-(ALG\d+)-TEST\.fwsc$/,
    description: { en: "Experimental SLOOP branch with DX7, VA and Karplus-Strong engines.", ru: "Экспериментальная ветка SLOOP с движками DX7, VA, Karplus-Strong." },
  },
];

const OFFICIAL = {
  id: "official", name: { en: "Official M-VAVE", ru: "Официальная M-VAVE" }, version: "V15", author: "M-VAVE",
  url: "https://yms-file-store.oss-cn-hongkong.aliyuncs.com/software/firmware/FM-1.fwsc",
  page: "https://www.m-vave.com/download",
  license: { en: "proprietary, loaded from the M-VAVE server", ru: "проприетарная, загружается с сервера M-VAVE" },
  description: { en: "Factory firmware: 6-operator DX7-style FM, effects, arpeggiator, sequencer.", ru: "Заводская прошивка: 6-операторный FM в духе DX7, эффекты, арпеджиатор, секвенсор." },
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

/** The newest release with a matching asset: { version, date, bytes, name }. */
async function fromRelease(source) {
  const headers = { accept: "application/vnd.github+json", "user-agent": "fm1-emulator-site" };
  if (process.env.GITHUB_TOKEN) headers.authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  const response = await fetch(`https://api.github.com/repos/${source.repo}/releases?per_page=20`, { headers });
  if (!response.ok) throw new Error(`releases: HTTP ${response.status}`);
  const releases = (await response.json())
    .filter((release) => !release.draft)
    .sort((a, b) => Date.parse(b.published_at ?? b.created_at) - Date.parse(a.published_at ?? a.created_at));
  for (const release of releases) {
    const asset = release.assets.find((a) => source.release.test(a.name));
    if (!asset) continue;
    const download = await fetch(asset.browser_download_url, { headers: { "user-agent": headers["user-agent"] } });
    if (!download.ok) throw new Error(`${asset.name}: HTTP ${download.status}`);
    return {
      version: release.tag_name.replace(/^v(?=\d)/, ""),
      date: (release.published_at ?? release.created_at).slice(0, 10),
      bytes: Buffer.from(await download.arrayBuffer()),
      extension: asset.name.match(/\.\w+$/)[0].toLowerCase(),
    };
  }
  throw new Error("no release with a firmware file");
}

/** The newest matching file in a branch: { version, date, bytes }. */
function fromBranch(source) {
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
    return {
      version: newest.version,
      date: git(dir, "log", "-1", "--format=%cs", "FETCH_HEAD").toString().trim(),
      bytes: git(dir, "show", `FETCH_HEAD:${newest.path}`),
      extension: ".fwsc",
    };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

mkdirSync(out, { recursive: true });
const catalog = [OFFICIAL];
for (const source of SOURCES) {
  try {
    const build = source.release ? await fromRelease(source) : fromBranch(source);
    const file = `${source.id}-${build.version.replace(/[^\w.-]/g, "_")}${build.extension}`;
    writeFileSync(join(out, file), build.bytes);
    catalog.push({
      id: source.id, name: source.name, version: build.version, author: source.author,
      description: source.description, file: `firmware/${file}`, size: build.bytes.length, date: build.date,
      license: source.license ?? "GPL-3.0", source: `https://github.com/${source.repo}`,
    });
    console.log(`${source.name} ${build.version} (${build.bytes.length} bytes, ${build.date})`);
  } catch (error) {
    // One unreachable source must not take the site down.
    console.warn(`${source.name}: skipped (${error.message.split("\n")[0]})`);
  }
}
writeFileSync(join(out, "catalog.json"), `${JSON.stringify(catalog, null, 2)}\n`);
console.log(`${catalog.length} entries in ${join(out, "catalog.json")}`);
