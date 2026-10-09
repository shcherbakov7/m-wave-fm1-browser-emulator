// SPDX-License-Identifier: GPL-3.0-only
// Page controller: device panel, input, firmware loading, audio and status.

// Panel geometry in the 1120×660 design space (from the upstream panel layout).
const W = 1120, H = 660;
const KNOBS = [
  [91, 106, "MASTER"], [208, 106, "SELECT"], [91, 224, "PRESETS"], [208, 224, "ALGORITHM"],
  [632, 106, "KNOB1"], [758, 106, "KNOB2"], [884, 106, "KNOB3"], [1010, 106, "KNOB4"],
];
const FUNCTION_LABELS = ["FX", "SEL", "ENV", "LFO", "EDIT", "GLO", "HOME", "SAVE", "ARP", "SEQ", "PLAY\nSTOP", "REC"];
const BLACK_LABELS = ["OP1", "OP2", "OP3", "OP4", "OP5", "OP6", "PIT", "GLO", "MONO", "POLY", ""];
const BLACK_NOTES = new Set([1, 3, 5, 8, 10, 13, 15, 17, 20, 22, 25]);
const HOME_ID = 8;

// Computer keyboard → panel control id. The 27-key keyboard starts on F.
const KEYBOARD = new Map();
const note = (index) => 14 + index;
// Lower whites Z…/ (notes 0–16) and blacks S D F H J L ;
[..."zxcvbnm,./"].forEach((k, i) => KEYBOARD.set(k, note([0, 2, 4, 6, 7, 9, 11, 12, 14, 16][i])));
[..."sdfhjl;"].forEach((k, i) => KEYBOARD.set(k, note([1, 3, 5, 8, 10, 13, 15][i])));
// Upper: 1 = note 17, whites Q…Y, blacks 3 4 6
KEYBOARD.set("1", note(17));
[..."qwerty"].forEach((k, i) => KEYBOARD.set(k, note([18, 19, 21, 23, 24, 26][i])));
[..."346"].forEach((k, i) => KEYBOARD.set(k, note([20, 22, 25][i])));
KEYBOARD.set("arrowleft", 0);
KEYBOARD.set("arrowright", 1);
KEYBOARD.set("escape", HOME_ID);

const $ = (id) => document.getElementById(id);
const device = $("device");
const lcd = $("lcd").getContext("2d");
const lcdImage = lcd.createImageData(240, 240);
const controls = new Map(); // id -> element
const held = new Map();     // id -> set of sources holding it

function place(element, x, y, w, h) {
  Object.assign(element.style, {
    left: `${(x / W) * 100}%`, top: `${(y / H) * 100}%`,
    width: `${(w / W) * 100}%`, height: `${(h / H) * 100}%`,
  });
  device.append(element);
  return element;
}

function buildPanel() {
  for (const [x, y, name] of KNOBS) {
    const knob = document.createElement("div");
    knob.className = "knob";
    knob.title = `${name}: ручки пока не эмулируются`;
    knob.innerHTML = `<span>${name}</span><div class="cap"></div>`;
    place(knob, x - 30, y - 62, 60, 92);
  }
  place(Object.assign(document.createElement("div"), { className: "well" }), 65, 286, 181, 56);
  ["OCT−", "OCT+"].forEach((label, id) => control(id, label, [75 + id * 82, 296, 70, 35]));
  place(Object.assign(document.createElement("div"), { className: "well" }), 598, 186, 448, 157);
  FUNCTION_LABELS.forEach((label, index) =>
    control(index + 2, label, [615 + (index % 6) * 71, 201 + Math.floor(index / 6) * 70, 57, 55]));
  const keys = place(Object.assign(document.createElement("div"), { className: "well" }), 40, 384, 1040, 238);
  keys.style.borderRadius = "40px";
  let white = 0, black = 0;
  for (let n = 0; n < 27; n++) {
    if (BLACK_NOTES.has(n)) {
      const label = BLACK_LABELS[black++];
      control(note(n), label, [58 + (white - 0.5) * 62.5, 405, 49, 94], "note");
    } else {
      control(note(n), "", [58 + white++ * 62.5, 510, 49, 94], "note white");
    }
  }
}

function control(id, label, [x, y, w, h], kind = "") {
  const button = document.createElement("button");
  button.className = `ctl ${kind}`;
  button.type = "button";
  button.dataset.id = id;
  if (kind.startsWith("note")) {
    if (label) button.innerHTML = `<small>${label}</small>`;
    button.setAttribute("aria-label", `Нота ${id - 13}${label ? ` (${label})` : ""}`);
  } else {
    button.textContent = label;
  }
  controls.set(id, place(button, x, y, w, h));
}

function press(id, source, down) {
  const sources = held.get(id) ?? new Set();
  const before = sources.size > 0;
  if (down) sources.add(source); else sources.delete(source);
  held.set(id, sources);
  const after = sources.size > 0;
  if (before === after) return;
  controls.get(id)?.classList.toggle("down", after);
  worker.postMessage({ type: "key", id, down: after });
}

function releaseAll() {
  for (const [id, sources] of held) {
    if (sources.size) { sources.clear(); controls.get(id)?.classList.remove("down"); worker.postMessage({ type: "key", id, down: false }); }
  }
}

// Pointer input: each pointer holds the control it went down on.
device.addEventListener("pointerdown", (event) => {
  const target = event.target.closest(".ctl");
  if (!target) return;
  event.preventDefault();
  target.setPointerCapture(event.pointerId);
  press(Number(target.dataset.id), `p${event.pointerId}`, true);
});
const pointerUp = (event) => {
  const target = event.target.closest?.(".ctl");
  if (target) press(Number(target.dataset.id), `p${event.pointerId}`, false);
};
device.addEventListener("pointerup", pointerUp);
device.addEventListener("pointercancel", pointerUp);
device.addEventListener("contextmenu", (event) => event.preventDefault());

addEventListener("keydown", (event) => {
  if (event.target.closest("input, select, textarea") || event.metaKey || event.ctrlKey || event.altKey) return;
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
const worker = new Worker("emulator-worker.js", { type: "module" });
let current = null; // { name, bytes }
let audio = null;   // { context, node }

worker.onmessage = ({ data }) => {
  switch (data.type) {
    case "ready": restoreLast(); break;
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
    case "serial": appendSerial(data.text); break;
    case "serial-busy": appendSerial("\n[консоль занята, повторите]\n"); break;
    case "status": showStatus(data.info); break;
    case "fault": setState(`Остановлено: ${data.message}`, "fault"); break;
    case "error": setState(`Ошибка${data.name ? ` (${data.name})` : ""}: ${data.message}`, "fault"); break;
  }
};
worker.postMessage({ type: "init", wasmUrl: new URL("fm1.wasm", location.href).href });

function setState(text, kind = "") {
  const state = $("state");
  state.textContent = text;
  state.className = `state ${kind}`;
}

function showStatus(info) {
  if (!info.loaded) return;
  const pct = info.realtime * 100;
  $("speed").textContent = info.realtime ? `${pct < 10 ? pct.toFixed(1) : pct.toFixed(0)}% реального` : "—";
  $("speed").title = `Загрузка эмулятора: ${(info.load * 100).toFixed(0)}% одного ядра`;
  $("steps").textContent = `${(info.steps / 1e6).toFixed(0)} M`;
  $("guest-time").textContent = `${info.guestSeconds.toFixed(1)} с`;
  $("irqs").textContent = info.irqs.toLocaleString("ru");
  if (info.paused && !info.fault) setState(`Пауза: ${current?.name ?? ""}`, "paused");
  else if (!info.fault && current) setState(`Работает: ${current.name}`, "running");
}

function appendSerial(text) {
  const pre = $("serial");
  pre.textContent = (pre.textContent + text).slice(-20000);
  pre.scrollTop = pre.scrollHeight;
}

// --- Firmware loading & recent list (IndexedDB) ----------------------------
function boot(name, bytes) {
  current = { name, bytes };
  releaseAll();
  $("serial").textContent = "";
  setState(`Загрузка ${name}…`);
  worker.postMessage({ type: "load", name, bytes: bytes.slice(0) });
  remember(name, bytes).then(refreshRecent);
}

async function openFile(file) {
  if (!file) return;
  if (file.size > 16 * 1024 * 1024) { setState("Файл слишком большой для прошивки FM-1", "fault"); return; }
  boot(file.name, await file.arrayBuffer());
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
$("recent").addEventListener("change", async (event) => {
  const name = event.target.value;
  event.target.value = "";
  const entry = name && (await database("readonly", (store) => store.get(name)));
  if (entry) boot(entry.name, entry.bytes);
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
    await context.audioWorklet.addModule("audio-worklet.js");
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
  const entries = (await database("readonly", (store) => store.getAll())) ?? [];
  entries.sort((a, b) => b.time - a.time);
  const select = $("recent");
  select.replaceChildren(new Option("Недавние…", ""), ...entries.map((e) => new Option(e.name, e.name)));
}

async function restoreLast() {
  await refreshRecent();
  setState("Загрузите прошивку FM-1 (.fwsc), например официальную V15 или Felucca");
}

buildPanel();
