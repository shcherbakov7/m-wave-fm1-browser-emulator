// SPDX-License-Identifier: GPL-3.0-only
// Page controller: device panel, input, firmware loading, audio and status.
// The build stamps ?v=<commit> on app.js; every file it loads carries the
// same stamp, so a new deployment is never mixed with cached old files.
const VERSION = new URL(import.meta.url).search;
const { createPanel, ID } = await import(`./panel.js${VERSION}`);

// Computer keyboard → panel control id. The 27-key keyboard starts on F.
const KEYBOARD = new Map();
const note = (index) => ID.NOTE + index;
// Lower row Z…/ and the upper row S D F H J L ; (notes 0–16)
[..."zxcvbnm,./"].forEach((k, i) => KEYBOARD.set(k, note([0, 2, 4, 6, 7, 9, 11, 12, 14, 16][i])));
[..."sdfhjl;"].forEach((k, i) => KEYBOARD.set(k, note([1, 3, 5, 8, 10, 13, 15][i])));
// Upper half: 1 = note 17, Q…Y, 3 4 6
KEYBOARD.set("1", note(17));
[..."qwerty"].forEach((k, i) => KEYBOARD.set(k, note([18, 19, 21, 23, 24, 26][i])));
[..."346"].forEach((k, i) => KEYBOARD.set(k, note([20, 22, 25][i])));
KEYBOARD.set("arrowleft", ID.OCT_DOWN);
KEYBOARD.set("arrowright", ID.OCT_UP);
KEYBOARD.set("escape", ID.HOME);
KEYBOARD.set(" ", ID.PLAY);
const KEY_NAMES = { arrowleft: "←", arrowright: "→", escape: "Esc", " ": "Пробел" };
const keyHints = new Map([...KEYBOARD].map(([key, id]) => [id, KEY_NAMES[key] ?? key.toUpperCase()]));

const $ = (id) => document.getElementById(id);
const device = $("device");
const lcd = $("lcd").getContext("2d");
const lcdImage = lcd.createImageData(240, 240);

const panel = createPanel(device, {
  keyHints,
  onControl: (id, down) => worker.postMessage({ type: "key", id, down }),
  onEncoder: (index, steps) => worker.postMessage({ type: "encoder", index, steps }),
  onMaster: (value) => worker.postMessage({ type: "master", value }),
});
const press = panel.press;
const releaseAll = panel.releaseAll;

addEventListener("keydown", (event) => {
  if (event.target.closest?.("input, select, textarea, .knob") || event.metaKey || event.ctrlKey || event.altKey) return;
  const id = KEYBOARD.get(event.key.toLowerCase());
  if (id === undefined) return;
  event.preventDefault();
  if (!event.repeat) press(id, `k${event.code}`, true);
});
addEventListener("keyup", (event) => {
  const id = KEYBOARD.get(event.key.toLowerCase());
  if (id !== undefined) press(id, `k${event.code}`, false);
});
addEventListener("blur", releaseAll);

// --- Emulator worker -------------------------------------------------------
const worker = new Worker(`emulator-worker.js${VERSION}`, { type: "module" });
let current = null; // { name, bytes }
let lastInfo = null; // latest worker status
let audio = null;   // { context, node }

worker.onmessage = ({ data }) => {
  switch (data.type) {
    case "ready": showEngine(); restoreLast(); break;
    case "loaded":
      setState(`Работает: ${data.name}`, "running");
      $("drop-hint").classList.add("hidden");
      $("pause").disabled = $("restart").disabled = false;
      $("pause").textContent = "Пауза";
      break;
    case "lcd":
      lcdImage.data.set(data.frame);
      lcd.putImageData(lcdImage, 0, 0);
      break;
    case "leds": panel.setLeds(data.leds); break;
    case "serial": appendSerial(data.text); break;
    case "serial-busy": appendSerial("\n[консоль занята, повторите]\n"); break;
    case "status": showStatus(data.info); break;
    case "fault":
      store.set("fm1-heartbeat", null);
      setState(`Остановлено: ${data.message}`, "fault");
      break;
    case "error":
      store.set("fm1-heartbeat", null);
      setState(`Ошибка${data.name ? ` (${data.name})` : ""}: ${data.message}`, "fault");
      break;
  }
};
// --- Engine mode and crash guard ---------------------------------------------
// The fast path compiles guest code to many small WebAssembly modules that
// call each other with tail calls. If a browser tab dies while the emulator
// runs (a heartbeat is left behind without a clean exit), the next visit
// steps down: no tail calls, then no translation at all. ?jit=0 and
// ?chain=0 force a mode; ?jit=1 resets it.
const MODES = [
  { name: "полный (JIT с цепочками)", jit: true, chaining: true },
  { name: "JIT без цепочек", jit: true, chaining: false },
  { name: "только интерпретатор (медленно)", jit: false, chaining: false },
];
const store = {
  get(key) { try { return JSON.parse(localStorage.getItem(key)); } catch { return null; } },
  set(key, value) { try { localStorage.setItem(key, JSON.stringify(value)); } catch { /* private mode */ } },
};
const params = new URL(location.href).searchParams;
let level = store.get("fm1-mode") ?? 0;
let modeNote = "";
// Other open tabs answer, so their heartbeat is not mistaken for a crash.
const tabs = new BroadcastChannel("fm1-tabs");
tabs.onmessage = ({ data }) => { if (data === "ping" && current) tabs.postMessage("pong"); };
const otherTabRunning = () => new Promise((resolve) => {
  const listener = ({ data }) => { if (data === "pong") resolve(true); };
  tabs.addEventListener("message", listener);
  tabs.postMessage("ping");
  setTimeout(() => { tabs.removeEventListener("message", listener); resolve(false); }, 400);
});
const beat = store.get("fm1-heartbeat");
if (beat && Date.now() - beat.time < 5 * 60_000 && beat.level >= level && level < MODES.length - 1
    && !(await otherTabRunning())) {
  level = beat.level + 1;
  const when = beat.seconds ? `через ${beat.seconds.toFixed(1)} с работы, блоков ${beat.blocks}` : "при запуске";
  modeNote = `Прошлый запуск (${beat.firmware ?? "прошивка"}, режим «${MODES[beat.level]?.name}») завершился аварийно ${when} — включён режим «${MODES[level].name}».`;
}
if (params.get("jit") === "0") level = 2;
else if (params.get("chain") === "0") level = Math.max(level, 1);
else if (params.get("jit") === "1") level = 0;
store.set("fm1-mode", level);
if (!beat || Date.now() - beat.time >= 5 * 60_000 || modeNote) store.set("fm1-heartbeat", null);
// WebKit (Safari, and every browser on iOS) has a small pool for compiled
// code: keep the number of translated blocks alive within it.
const webkit = /iPhone|iPad|iPod/.test(navigator.userAgent)
  || (/AppleWebKit/.test(navigator.userAgent) && !/Chrome|Chromium|Edg/.test(navigator.userAgent));
const mode = { ...MODES[level], blockLimit: webkit ? 1000 : 0 };
// While a firmware starts or runs, leave a heartbeat (with how far it got,
// for the notice); a clean exit or a reported stop removes it. It is left
// before loading, as most code is compiled in the first seconds.
function heartbeat() {
  store.set("fm1-heartbeat", {
    time: Date.now(), level, firmware: current.name,
    seconds: lastInfo?.guestSeconds ?? 0, blocks: lastInfo?.jit?.compiled ?? 0,
  });
}
setInterval(() => {
  if (current && $("state").classList.contains("running")) heartbeat();
}, 1000);
addEventListener("pagehide", () => store.set("fm1-heartbeat", null));

worker.postMessage({ type: "init", wasmUrl: new URL(`fm1.wasm${VERSION}`, location.href).href, mode });

function setState(text, kind = "") {
  const state = $("state");
  state.textContent = text;
  state.className = `state ${kind}`;
}

function showStatus(info) {
  if (!info.loaded) return;
  lastInfo = info;
  const pct = info.realtime * 100;
  $("speed").textContent = info.realtime ? `${pct < 10 ? pct.toFixed(1) : pct.toFixed(0)}% реального` : "—";
  $("speed").title = `Загрузка эмулятора: ${(info.load * 100).toFixed(0)}% одного ядра`;
  $("steps").textContent = `${(info.steps / 1e6).toFixed(0)} M`;
  $("guest-time").textContent = `${info.guestSeconds.toFixed(1)} с`;
  $("irqs").textContent = info.irqs.toLocaleString("ru");
  showEngine(info);
  if (info.paused && !info.fault) setState(`Пауза: ${current?.name ?? ""}`, "paused");
  else if (!info.fault && current) setState(`Работает: ${current.name}`, "running");
}

/** Engine mode and browser, for reports (and the crash-guard notice). */
function showEngine(info = null) {
  const jit = info?.jit;
  const browser = navigator.userAgent.match(/(Version\/[\d.]+.*Safari|Chrome\/[\d.]+|Firefox\/[\d.]+|Edg\/[\d.]+)/)?.[0] ?? "";
  const platform = /iPhone|iPad/.test(navigator.userAgent) ? "iOS" : /Android/.test(navigator.userAgent) ? "Android" : "";
  const details = jit ? ` · блоков ${jit.compiled}${mode.blockLimit ? ` (не больше ${mode.blockLimit})` : ""}, цепочки ${jit.chaining ? "вкл" : "выкл"}` : "";
  $("engine").innerHTML = `${modeNote ? `<span class="engine-warn">${escape(modeNote)}</span><br>` : ""}
    Режим: ${escape(mode.name)}${details} · ${escape([platform, browser].filter(Boolean).join(" "))}
    ${level ? ` · <a href="?jit=1">вернуть полный режим</a>` : ""}`;
}

function appendSerial(text) {
  const pre = $("serial");
  pre.textContent = (pre.textContent + text).slice(-20000);
  pre.scrollTop = pre.scrollHeight;
}

// --- Firmware: ready-made catalog, own files, recent list (IndexedDB) -------
let catalog = [];   // web/firmware/catalog.json (tools/update-firmware.mjs)

/** Start `bytes`; `entry` is the catalog entry it came from, if any. */
function boot(name, bytes, entry = null) {
  current = { name, bytes, entry };
  lastInfo = null;
  heartbeat();
  releaseAll();
  $("serial").textContent = "";
  setState(`Загрузка ${name}…`);
  showAbout(entry);
  worker.postMessage({ type: "load", name, bytes: bytes.slice(0) });
  if (!entry) remember(name, bytes).then(refreshRecent);
  const url = new URL(location.href);
  if (entry) url.searchParams.set("fw", entry.id); else url.searchParams.delete("fw");
  history.replaceState(null, "", url);
  for (const element of document.querySelectorAll(".fw.active")) element.classList.remove("active");
  refreshRecent();
}

async function openFile(file) {
  if (!file) return;
  if (file.size > 16 * 1024 * 1024) { setState("Файл слишком большой для прошивки FM-1", "fault"); return; }
  boot(file.name, await file.arrayBuffer());
}

const escape = (text) => String(text).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

function showAbout(entry, error = "") {
  const about = $("about");
  about.hidden = !entry;
  if (!entry) return;
  const link = entry.source ?? entry.page;
  about.innerHTML = `${error ? `<p class="about-error">${error}</p>` : ""}
    <strong>${escape(entry.name)} ${escape(entry.version)}</strong> — ${escape(entry.author)}<br>
    ${escape(entry.description)}<br>
    ${entry.check ? `<small>${entry.check.ok ? "✓ проверена в эмуляторе при сборке сайта" : `⚠ в эмуляторе останавливается: ${escape(entry.check.stop || "экран пуст")}`}</small><br>` : ""}
    <small>${escape(entry.license)}${entry.date ? ` · ${escape(entry.date)}` : ""} ·
    <a href="${escape(link)}" target="_blank" rel="noopener">${entry.source ? "исходники и инструкции" : "страница загрузки"}</a></small>`;
}

/** Download a catalog firmware and start it. */
async function bootCatalog(entry) {
  const name = `${entry.name} ${entry.version}`;
  setState(`Скачивание ${name}…`);
  showAbout(entry);
  try {
    const response = await fetch(entry.file ?? entry.url, { cache: "force-cache" });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    boot(name, await response.arrayBuffer(), entry);
  } catch (error) {
    if (current) {
      showAbout(entry, `Не удалось скачать ${escape(name)}, работает прежняя прошивка. ` + (entry.url
        ? `Сервер M-VAVE не отдаёт файл другим сайтам: скачайте <a href="${escape(entry.url)}">FM-1.fwsc</a> и откройте его кнопкой «Свой файл».`
        : escape(String(error))));
      return;
    }
    setState(`Не удалось скачать ${name}`, "fault");
    showAbout(entry, entry.url
      ? `Сервер M-VAVE не отдаёт файл другим сайтам. Скачайте <a href="${escape(entry.url)}">FM-1.fwsc</a> с официального сервера и откройте его кнопкой «Свой файл» — дальше он будет в списке «Недавние».`
      : escape(String(error)));
  }
}

async function loadCatalog() {
  try {
    const response = await fetch("firmware/catalog.json", { cache: "no-cache" });
    catalog = response.ok ? await response.json() : [];
  } catch {
    catalog = [];
  }
}

$("file").addEventListener("change", (event) => openFile(event.target.files[0]));
for (const type of ["dragenter", "dragover"]) {
  document.addEventListener(type, (event) => { event.preventDefault(); device.classList.add("dragging"); });
}
document.addEventListener("dragleave", (event) => { if (!event.relatedTarget) device.classList.remove("dragging"); });
document.addEventListener("drop", (event) => {
  event.preventDefault();
  device.classList.remove("dragging");
  openFile(event.dataTransfer.files[0]);
});

$("pause").addEventListener("click", () => {
  const paused = $("pause").textContent === "Пауза";
  $("pause").textContent = paused ? "Продолжить" : "Пауза";
  worker.postMessage({ type: "pause", paused });
});
$("restart").addEventListener("click", () => current && boot(current.name, current.bytes));
$("firmware").addEventListener("change", async (event) => {
  const [kind, key] = [event.target.value.slice(0, 2), event.target.value.slice(2)];
  event.target.value = "";
  if (kind === "c:") {
    const entry = catalog.find((e) => e.id === key);
    if (entry) bootCatalog(entry);
  } else if (kind === "r:") {
    const saved = await database("readonly", (store) => store.get(key));
    if (saved) boot(saved.name, saved.bytes);
  }
});

$("serial-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const input = $("serial-input");
  worker.postMessage({ type: "serial", text: `${input.value}\r\n` });
  input.value = "";
});

$("sound").addEventListener("click", async () => {
  if (!audio) {
    const context = new AudioContext({ sampleRate: 44100 });
    await context.audioWorklet.addModule(`audio-worklet.js${VERSION}`);
    const node = new AudioWorkletNode(context, "fm1-output", { outputChannelCount: [2] });
    node.connect(context.destination);
    // The worker feeds the worklet directly, bypassing this thread.
    const { port1, port2 } = new MessageChannel();
    node.port.postMessage({ port: port1 }, [port1]);
    worker.postMessage({ type: "audio-port", port: port2 }, [port2]);
    audio = { context, node };
    $("sound").textContent = "🔊 Звук";
  } else if (audio.context.state === "running") {
    await audio.context.suspend();
    $("sound").textContent = "🔇 Звук";
  } else {
    await audio.context.resume();
    $("sound").textContent = "🔊 Звук";
  }
});

function database(mode, action) {
  return new Promise((resolve) => {
    let request;
    try { request = indexedDB.open("fm1-emulator", 1); } catch { resolve(null); return; }
    request.onupgradeneeded = () => request.result.createObjectStore("firmware", { keyPath: "name" });
    request.onerror = () => resolve(null);
    request.onsuccess = () => {
      const db = request.result;
      const tx = db.transaction("firmware", mode);
      const result = action(tx.objectStore("firmware"));
      tx.oncomplete = () => { resolve(result?.result ?? null); db.close(); };
      tx.onerror = () => { resolve(null); db.close(); };
    };
  });
}
const remember = (name, bytes) => database("readwrite", (store) => store.put({ name, bytes, time: Date.now() }));

async function refreshRecent() {
  const saved = (await database("readonly", (store) => store.getAll())) ?? [];
  saved.sort((a, b) => b.time - a.time);
  const group = (label, options) => {
    const element = document.createElement("optgroup");
    element.label = label;
    element.append(...options);
    return element;
  };
  const official = catalog.filter((e) => e.url), open = catalog.filter((e) => e.file);
  const groups = [new Option("Выбрать прошивку…", "")];
  if (official.length) groups.push(group("Официальная", official.map((e) => new Option(`${e.name} ${e.version}`, `c:${e.id}`))));
  const mark = (e) => (e.check && !e.check.ok ? " ⚠" : "");
  if (open.length) groups.push(group("Открытые прошивки", open.map((e) => new Option(`${e.name} ${e.version} — ${e.author}${mark(e)}`, `c:${e.id}`))));
  if (saved.length) groups.push(group("Ваши файлы (недавние)", saved.map((e) => new Option(e.name, `r:${e.name}`))));
  $("firmware").replaceChildren(...groups);
  showFirmwareList(saved);
}

/** The firmware card: a button per firmware, easy to tap on a phone. */
function showFirmwareList(saved = []) {
  const button = (title, detail, onClick, extra = "") => {
    const element = document.createElement("button");
    element.type = "button";
    element.className = `fw ${extra}`;
    element.innerHTML = `<span>${escape(title)}</span><small>${escape(detail)}</small>`;
    element.addEventListener("click", onClick);
    return element;
  };
  const items = catalog.map((entry) => {
    const active = current?.entry?.id === entry.id ? "active" : "";
    const warn = entry.check && !entry.check.ok ? " ⚠" : "";
    return button(`${entry.name}${warn}`, `${entry.version} · ${entry.author}`, () => bootCatalog(entry), active);
  });
  for (const file of saved.slice(0, 4)) {
    items.push(button(file.name, "ваш файл", async () => {
      const entry = await database("readonly", (store) => store.get(file.name));
      if (entry) boot(entry.name, entry.bytes);
    }, current?.name === file.name ? "active" : ""));
  }
  items.push(button("Свой файл…", ".fwsc с устройства", () => $("file").click(), "own"));
  $("firmware-list").replaceChildren(...items);
}

async function restoreLast() {
  await loadCatalog();
  await refreshRecent();
  // ?fw=felucca opens that firmware straight away (shareable links).
  const wanted = new URL(location.href).searchParams.get("fw");
  const entry = wanted && catalog.find((e) => e.id === wanted);
  if (entry) bootCatalog(entry);
  else setState("Выберите прошивку в списке сверху или откройте свой файл .fwsc");
}

