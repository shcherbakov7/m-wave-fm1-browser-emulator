// SPDX-License-Identifier: GPL-3.0-only
// The FM-1 front panel: body, screen bezel, knobs, buttons and key pads drawn
// to the proportions of the device, with pointer, wheel and keyboard input
// and the LEDs under the buttons and pads.
//
// Coordinates are in a 1288×740 design space traced from a photo of the
// panel; elements are placed in percentages so the panel scales freely.

const W = 1288, H = 740;

/** Control IDs, as the emulator numbers them (gpio.rs PANEL_KEYMAP). */
export const ID = {
  OCT_DOWN: 0, OCT_UP: 1, FX: 2, SEL: 3, ENV: 4, LFO: 5, EDIT: 6, GLO: 7,
  HOME: 8, SAVE: 9, ARP: 10, SEQ: 11, PLAY: 12, REC: 13, NOTE: 14,
};

// Rotary encoders in the emulator's order (gpio.rs PANEL_ENCODERS).
const ENCODERS = ["SELECT", "PRESETS", "ALGORITHM", "KNOB1", "KNOB2", "KNOB3", "KNOB4"];
// Knob centres; MASTER is the potentiometer, the others are encoders.
const KNOBS = [
  { name: "MASTER", x: 88, y: 99 },
  { name: "SELECT", x: 230, y: 99, encoder: 0 },
  { name: "PRESETS", x: 88, y: 236, encoder: 1 },
  { name: "ALGORITHM", x: 230, y: 236, encoder: 2 },
  { name: "KNOB1", x: 725, y: 99, encoder: 3 },
  { name: "KNOB2", x: 870, y: 99, encoder: 4 },
  { name: "KNOB3", x: 1020, y: 99, encoder: 5 },
  { name: "KNOB4", x: 1168, y: 99, encoder: 6 },
];
const BUTTONS = [
  ["FX", ID.FX], ["SEL", ID.SEL], ["ENV", ID.ENV], ["LFO", ID.LFO], ["EDIT", ID.EDIT], ["GLO", ID.GLO],
  ["HOME", ID.HOME], ["SAVE", ID.SAVE], ["ARP", ID.ARP], ["SEQ", ID.SEQ], ["PLAY\nSTOP", ID.PLAY], ["REC", ID.REC],
];
// The 27 pads from F3: which are in the upper row, and their printed labels.
const UPPER = new Set([1, 3, 5, 8, 10, 13, 15, 17, 20, 22, 25]);
const UPPER_LABELS = ["OP1", "OP2", "OP3", "OP4", "OP5", "OP6", "PIT", "GLO", "MONO", "POLY", ""];
const NOTE_NAMES = ["F", "F♯", "G", "G♯", "A", "A♯", "B", "C", "C♯", "D", "D♯", "E"];
const LED_COLOR = { [ID.PLAY]: "green", [ID.REC]: "red" };

const MASTER_MAX = 1023;
const MASTER_SWEEP = 300;      // degrees from minimum to maximum
const DETENT_DEGREES = 15;     // one encoder click, as drawn
const DRAG_PIXELS_PER_DETENT = 9;

/**
 * Build the panel inside `device` (an empty element). Callbacks:
 *   onControl(id, down), onEncoder(index, steps), onMaster(value 0..1023).
 * Returns { setLeds(Uint8Array), releaseAll(), press(id, source, down), keyIds }.
 */
export function createPanel(device, { onControl, onEncoder, onMaster, keyHints = new Map() }) {
  const controls = new Map(); // id -> element
  const held = new Map();     // id -> Set of sources holding it

  const place = (element, x, y, w, h) => {
    Object.assign(element.style, {
      left: `${(x / W) * 100}%`, top: `${(y / H) * 100}%`,
      width: `${(w / W) * 100}%`, height: `${(h / H) * 100}%`,
    });
    device.append(element);
    return element;
  };
  const div = (className, text = "") => Object.assign(document.createElement("div"), { className, textContent: text });

  // Wells, screen and printed labels.
  place(div("well"), 52, 312, 210, 64);
  place(div("well"), 685, 192, 528, 181);
  place(div("well keys-well"), 22, 420, 1243, 292);
  const bezel = place(div("bezel"), 320, 36, 316, 306);
  bezel.append(device.querySelector("#lcd-frame") ?? div(""));

  // Knobs.
  let master = MASTER_MAX >> 1;
  for (const knob of KNOBS) {
    const label = place(div("print"), knob.x - 80, knob.y - 72, 160, 26);
    label.textContent = knob.name;
    const element = place(div("knob"), knob.x - 36, knob.y - 36, 72, 72);
    element.tabIndex = 0;
    element.setAttribute("role", "slider");
    element.setAttribute("aria-label", knob.name);
    const cap = div("cap");
    cap.append(div("mark"));
    element.append(cap);
    let angle = knob.encoder === undefined ? -MASTER_SWEEP / 2 + (MASTER_SWEEP * master) / MASTER_MAX : 0;
    const draw = () => { cap.style.transform = `rotate(${angle}deg)`; };
    const turn = (steps) => {
      if (!steps) return;
      if (knob.encoder === undefined) {
        master = Math.max(0, Math.min(MASTER_MAX, master + steps * 16));
        angle = -MASTER_SWEEP / 2 + (MASTER_SWEEP * master) / MASTER_MAX;
        element.setAttribute("aria-valuenow", String(master));
        element.title = `MASTER: ${Math.round((master / MASTER_MAX) * 100)}% — тяните вверх/вниз или крутите колёсико`;
        onMaster(master);
      } else {
        angle += steps * DETENT_DEGREES;
        onEncoder(knob.encoder, steps);
      }
      draw();
    };
    if (knob.encoder === undefined) {
      element.setAttribute("aria-valuemin", "0");
      element.setAttribute("aria-valuemax", String(MASTER_MAX));
      element.setAttribute("aria-valuenow", String(master));
      element.title = "MASTER (громкость) — тяните вверх/вниз или крутите колёсико";
    } else {
      element.title = `${knob.name} — энкодер: тяните вверх/вниз или крутите колёсико`;
    }
    draw();

    // Drag: up or right is clockwise.
    let drag = null;
    element.addEventListener("pointerdown", (event) => {
      event.preventDefault();
      element.setPointerCapture(event.pointerId);
      drag = { x: event.clientX, y: event.clientY, rest: 0 };
      element.classList.add("turning");
    });
    element.addEventListener("pointermove", (event) => {
      if (!drag) return;
      const scale = W / device.clientWidth; // keep the feel independent of panel size
      drag.rest += ((event.clientX - drag.x) - (event.clientY - drag.y)) * scale;
      drag.x = event.clientX; drag.y = event.clientY;
      const steps = Math.trunc(drag.rest / DRAG_PIXELS_PER_DETENT);
      drag.rest -= steps * DRAG_PIXELS_PER_DETENT;
      turn(steps);
    });
    const end = () => { drag = null; element.classList.remove("turning"); };
    element.addEventListener("pointerup", end);
    element.addEventListener("pointercancel", end);
    // Wheel: one detent per notch, trackpads by distance.
    let wheelRest = 0;
    element.addEventListener("wheel", (event) => {
      event.preventDefault();
      wheelRest += event.deltaMode === 0 ? -event.deltaY / 40 : -Math.sign(event.deltaY);
      const steps = Math.trunc(wheelRest);
      wheelRest -= steps;
      turn(steps);
    }, { passive: false });
    element.addEventListener("keydown", (event) => {
      const steps = { ArrowUp: 1, ArrowRight: 1, ArrowDown: -1, ArrowLeft: -1, PageUp: 4, PageDown: -4 }[event.key];
      if (steps === undefined) return;
      event.preventDefault();
      event.stopPropagation();
      turn(steps);
    });
  }

  // Buttons.
  const control = (id, label, [x, y, w, h], kind) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = `ctl ${kind}`;
    button.dataset.id = id;
    if (LED_COLOR[id]) button.dataset.led = LED_COLOR[id];
    const hint = keyHints.get(id);
    if (kind.startsWith("btn")) {
      button.append(div("legend", label));
      button.setAttribute("aria-label", label.replace("\n", " / "));
      if (hint) button.title = `${label.replace("\n", "/")} — клавиша ${hint}`;
    } else {
      button.append(div("groove"));
      if (label) button.append(div("legend", label));
      const n = id - ID.NOTE;
      const name = `${NOTE_NAMES[n % 12]}${3 + Math.floor((n + 5) / 12)}`;
      button.setAttribute("aria-label", `Клавиша ${name}${label ? ` (${label})` : ""}`);
      button.title = `${name}${label ? ` · ${label}` : ""}${hint ? ` — клавиша ${hint}` : ""}`;
    }
    controls.set(id, place(button, x, y, w, h));
  };
  control(ID.OCT_DOWN, "OCT−", [64, 322, 78, 42], "btn oct");
  control(ID.OCT_UP, "OCT+", [170, 322, 78, 42], "btn oct");
  BUTTONS.forEach(([label, id], i) => control(id, label, [712 + (i % 6) * 82, 208 + Math.floor(i / 6) * 82, 64, 62], "btn"));
  const LOWER_X0 = 75, LOWER_PITCH = 75.5;
  let lower = 0, upper = 0;
  for (let n = 0; n < 27; n++) {
    if (UPPER.has(n)) {
      const x = LOWER_X0 + (lower - 0.5) * LOWER_PITCH;
      control(ID.NOTE + n, UPPER_LABELS[upper++], [x - 33, 440, 66, 122], "pad upper");
    } else {
      control(ID.NOTE + n, "", [LOWER_X0 + lower++ * LOWER_PITCH - 33, 570, 66, 128], "pad");
    }
  }
  // Dots on the B–C and E–F boundaries.
  for (const boundary of [3.5, 6.5, 10.5, 13.5]) place(div("dot"), LOWER_X0 + boundary * LOWER_PITCH - 3, 563, 6, 6);

  // Pressing: a control is down while any source (pointer, key) holds it.
  function press(id, source, down) {
    const sources = held.get(id) ?? new Set();
    const before = sources.size > 0;
    if (down) sources.add(source); else sources.delete(source);
    held.set(id, sources);
    const after = sources.size > 0;
    if (before === after) return;
    controls.get(id)?.classList.toggle("down", after);
    onControl(id, after);
  }
  function releaseAll() {
    for (const [id, sources] of held) {
      if (!sources.size) continue;
      sources.clear();
      controls.get(id)?.classList.remove("down");
      onControl(id, false);
    }
  }

  // Pointers: a button holds while the pointer stays down; on the pads a
  // pointer slides from key to key.
  const pointerKey = new Map(); // pointerId -> id held
  device.addEventListener("pointerdown", (event) => {
    const target = event.target.closest(".ctl");
    if (!target) return;
    event.preventDefault();
    const id = Number(target.dataset.id);
    if (!target.classList.contains("pad")) target.setPointerCapture(event.pointerId);
    else { device.setPointerCapture(event.pointerId); gliding.add(event.pointerId); }
    pointerKey.set(event.pointerId, id);
    press(id, `p${event.pointerId}`, true);
  });
  const gliding = new Set(); // pointers that went down on a pad
  device.addEventListener("pointermove", (event) => {
    if (!gliding.has(event.pointerId)) return;
    const id = pointerKey.get(event.pointerId);
    const under = document.elementFromPoint(event.clientX, event.clientY);
    const pad = under?.closest?.(".pad");
    // Between two pads the note holds; off the keyboard it ends.
    const next = pad ? Number(pad.dataset.id) : under?.closest?.(".keys-well") ? id : -1;
    if (next === id) return;
    if (id >= 0) press(id, `p${event.pointerId}`, false);
    pointerKey.set(event.pointerId, next);
    if (next >= 0) press(next, `p${event.pointerId}`, true);
  });
  const pointerUp = (event) => {
    const id = pointerKey.get(event.pointerId);
    pointerKey.delete(event.pointerId);
    gliding.delete(event.pointerId);
    if (id !== undefined && id >= 0) press(id, `p${event.pointerId}`, false);
  };
  device.addEventListener("pointerup", pointerUp);
  device.addEventListener("pointercancel", pointerUp);
  device.addEventListener("lostpointercapture", pointerUp);
  device.addEventListener("contextmenu", (event) => event.preventDefault());

  // LEDs: brightness per control ID, 0..255.
  function setLeds(leds) {
    for (const [id, element] of controls) {
      const level = leds[id] / 255;
      // Perceived brightness: a short duty cycle still reads as a glow.
      element.style.setProperty("--led", level ? Math.min(1, Math.sqrt(level)).toFixed(3) : "0");
    }
  }

  return { setLeds, releaseAll, press };
}
