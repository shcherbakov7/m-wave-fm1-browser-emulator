// SPDX-License-Identifier: GPL-3.0-only
// Interface languages. `t(key, params)` looks a string up in the current
// language (falling back to English); `{name}` is replaced by params.name.
// Static page text carries data-i18n (text), data-i18n-html (markup from
// this file only), data-i18n-title, -placeholder and -aria-label.

export const LANGUAGES = { en: "EN", ru: "RU" };

const STRINGS = {
  en: {
    "meta.description": "Browser emulator of the M-VAVE FM-1: runs unmodified .fwsc firmware",
    "title.suffix": "emulator",
    "lang.label": "Interface language",
    "firmware.title": "Ready-made firmware and files you opened before",
    "firmware.choose": "Choose firmware…",
    "firmware.official": "Official",
    "firmware.open": "Open-source firmware",
    "firmware.recent": "Your files (recent)",
    "file.button": "Own file",
    "file.title": "Open your own firmware file (.fwsc, .ufw, .bin, .elf)",
    "pause": "Pause",
    "resume": "Resume",
    "restart": "Restart",
    "sound": "Sound",
    "sound.title": "Turn sound on or off",
    "device.label": "FM-1 panel",
    "lcd.label": "Screen",
    "drop.hint": "Choose firmware in the list above<br><small>or drop your .fwsc file here</small>",
    "list.title": "Firmware",
    "list.yours": "your file",
    "list.own": "Own file…",
    "list.own.detail": ".fwsc from your device",
    "stats.speed": "Speed",
    "stats.steps": "CPU steps",
    "stats.time": "Device time",
    "stats.irqs": "Interrupts",
    "speed.value": "{pct}% of real time",
    "speed.title": "Emulator load: {pct}% of one core",
    "seconds": "{value} s",
    "help.title": "Controls",
    "help.html": `<p>Buttons and keys: mouse or touch (hold them; you can slide a finger along the keys). Knobs: drag up/down, turn the mouse wheel, or use the arrow keys when focused. MASTER is the volume, the other seven knobs are encoders.</p>
<p>Computer keyboard:</p>
<ul>
  <li><kbd>Z</kbd>…<kbd>/</kbd> and <kbd>S</kbd> <kbd>D</kbd> <kbd>F</kbd> <kbd>H</kbd> <kbd>J</kbd> <kbd>L</kbd> <kbd>;</kbd> — keys from F3</li>
  <li><kbd>1</kbd> <kbd>Q</kbd>…<kbd>Y</kbd> and <kbd>3</kbd> <kbd>4</kbd> <kbd>6</kbd> — the upper part</li>
  <li><kbd>←</kbd> <kbd>→</kbd> — OCT− / OCT+, <kbd>Esc</kbd> — HOME, <kbd>Space</kbd> — PLAY/STOP</li>
</ul>
<p class="note">Button and key lights follow the device's LEDs.</p>`,
    "console.title": "Firmware USB console",
    "console.placeholder": "command (e.g. help) and Enter",
    "console.busy": "\n[console busy, try again]\n",
    "footer.html": `GPL-3.0 · Core based on <a href="https://github.com/simonjohansson/fm1-emulator">fm1-emulator</a> ·
    <a href="https://github.com/shcherbakov7/m-wave-fm1-browser-emulator">Source code</a>`,
    "state.none": "No firmware",
    "state.choose": "Choose firmware in the list above or open your own .fwsc file",
    "state.running": "Running: {name}",
    "state.paused": "Paused: {name}",
    "state.loading": "Loading {name}…",
    "state.downloading": "Downloading {name}…",
    "state.download-failed": "Could not download {name}",
    "state.fault": "Stopped: {message}",
    "state.error": "Error: {message}",
    "state.error-named": "Error ({name}): {message}",
    "state.too-big": "File too large for FM-1 firmware",
    "key.space": "Space",
    "mode.full": "full (JIT with chaining)",
    "mode.nochain": "JIT without chaining",
    "mode.interpreter": "interpreter only (slow)",
    "engine.mode": "Mode: {mode}",
    "engine.blocks": "{count} blocks",
    "engine.limit": "(at most {limit})",
    "engine.chaining-on": "chaining on",
    "engine.chaining-off": "chaining off",
    "engine.reset": "back to full mode",
    "crash.at-start": "while starting",
    "crash.after": "after {seconds} s, {blocks} blocks",
    "crash.note": "The last run ({firmware}, mode “{mode}”) crashed {when}: switched to mode “{next}”.",
    "crash.firmware": "firmware",
    "about.checked": "✓ checked in the emulator when the site was built",
    "about.stops": "⚠ stops in the emulator: {reason}",
    "about.blank": "blank screen",
    "about.source": "source and instructions",
    "about.page": "download page",
    "download.failed-running": "Could not download {name}; the previous firmware keeps running.",
    "download.mvave-short": `The M-VAVE server does not give the file to other sites: download <a href="{url}">FM-1.fwsc</a> and open it with “Own file”.`,
    "download.mvave": `The M-VAVE server does not give the file to other sites. Download <a href="{url}">FM-1.fwsc</a> from the official server and open it with “Own file”; after that it stays in the recent list.`,
    "panel.master": "MASTER (volume): drag up/down or turn the wheel",
    "panel.master-value": "MASTER: {pct}%: drag up/down or turn the wheel",
    "panel.encoder": "{name}: encoder, drag up/down or turn the wheel",
    "panel.shortcut": "key {key}",
    "panel.key": "Key {name}",
  },
  ru: {
    "meta.description": "Браузерный эмулятор M-VAVE FM-1: запускает немодифицированные прошивки .fwsc",
    "title.suffix": "эмулятор",
    "lang.label": "Язык интерфейса",
    "firmware.title": "Готовые прошивки и ранее загруженные файлы",
    "firmware.choose": "Выбрать прошивку…",
    "firmware.official": "Официальная",
    "firmware.open": "Открытые прошивки",
    "firmware.recent": "Ваши файлы (недавние)",
    "file.button": "Свой файл",
    "file.title": "Открыть свой файл прошивки (.fwsc, .ufw, .bin, .elf)",
    "pause": "Пауза",
    "resume": "Продолжить",
    "restart": "Перезапуск",
    "sound": "Звук",
    "sound.title": "Включить или выключить звук",
    "device.label": "Панель FM-1",
    "lcd.label": "Экран",
    "drop.hint": "Выберите прошивку в списке сверху<br><small>или перетащите сюда свой файл .fwsc</small>",
    "list.title": "Прошивки",
    "list.yours": "ваш файл",
    "list.own": "Свой файл…",
    "list.own.detail": ".fwsc с устройства",
    "stats.speed": "Скорость",
    "stats.steps": "Шаги CPU",
    "stats.time": "Время устройства",
    "stats.irqs": "Прерывания",
    "speed.value": "{pct}% реального",
    "speed.title": "Загрузка эмулятора: {pct}% одного ядра",
    "seconds": "{value} с",
    "help.title": "Управление",
    "help.html": `<p>Кнопки и клавиши — мышью или касанием (удерживайте; по клавишам можно вести пальцем). Ручки — тяните вверх/вниз, крутите колёсико мыши или стрелками, наведя фокус. MASTER — громкость, остальные семь ручек — энкодеры.</p>
<p>Клавиатура компьютера:</p>
<ul>
  <li><kbd>Z</kbd>…<kbd>/</kbd> и <kbd>S</kbd> <kbd>D</kbd> <kbd>F</kbd> <kbd>H</kbd> <kbd>J</kbd> <kbd>L</kbd> <kbd>;</kbd> — клавиши с F3</li>
  <li><kbd>1</kbd> <kbd>Q</kbd>…<kbd>Y</kbd> и <kbd>3</kbd> <kbd>4</kbd> <kbd>6</kbd> — верхняя часть</li>
  <li><kbd>←</kbd> <kbd>→</kbd> — OCT− / OCT+, <kbd>Esc</kbd> — HOME, <kbd>Пробел</kbd> — PLAY/STOP</li>
</ul>
<p class="note">Подсветка кнопок и клавиш повторяет светодиоды устройства.</p>`,
    "console.title": "USB-консоль прошивки",
    "console.placeholder": "команда (например help) и Enter",
    "console.busy": "\n[консоль занята, повторите]\n",
    "footer.html": `GPL-3.0 · Ядро основано на <a href="https://github.com/simonjohansson/fm1-emulator">fm1-emulator</a> ·
    <a href="https://github.com/shcherbakov7/m-wave-fm1-browser-emulator">Исходный код</a>`,
    "state.none": "Нет прошивки",
    "state.choose": "Выберите прошивку в списке сверху или откройте свой файл .fwsc",
    "state.running": "Работает: {name}",
    "state.paused": "Пауза: {name}",
    "state.loading": "Загрузка {name}…",
    "state.downloading": "Скачивание {name}…",
    "state.download-failed": "Не удалось скачать {name}",
    "state.fault": "Остановлено: {message}",
    "state.error": "Ошибка: {message}",
    "state.error-named": "Ошибка ({name}): {message}",
    "state.too-big": "Файл слишком большой для прошивки FM-1",
    "key.space": "Пробел",
    "mode.full": "полный (JIT с цепочками)",
    "mode.nochain": "JIT без цепочек",
    "mode.interpreter": "только интерпретатор (медленно)",
    "engine.mode": "Режим: {mode}",
    "engine.blocks": "блоков {count}",
    "engine.limit": "(не больше {limit})",
    "engine.chaining-on": "цепочки вкл",
    "engine.chaining-off": "цепочки выкл",
    "engine.reset": "вернуть полный режим",
    "crash.at-start": "при запуске",
    "crash.after": "через {seconds} с работы, блоков {blocks}",
    "crash.note": "Прошлый запуск ({firmware}, режим «{mode}») завершился аварийно {when} — включён режим «{next}».",
    "crash.firmware": "прошивка",
    "about.checked": "✓ проверена в эмуляторе при сборке сайта",
    "about.stops": "⚠ в эмуляторе останавливается: {reason}",
    "about.blank": "экран пуст",
    "about.source": "исходники и инструкции",
    "about.page": "страница загрузки",
    "download.failed-running": "Не удалось скачать {name}, работает прежняя прошивка.",
    "download.mvave-short": `Сервер M-VAVE не отдаёт файл другим сайтам: скачайте <a href="{url}">FM-1.fwsc</a> и откройте его кнопкой «Свой файл».`,
    "download.mvave": `Сервер M-VAVE не отдаёт файл другим сайтам. Скачайте <a href="{url}">FM-1.fwsc</a> с официального сервера и откройте его кнопкой «Свой файл» — дальше он будет в списке «Недавние».`,
    "panel.master": "MASTER (громкость) — тяните вверх/вниз или крутите колёсико",
    "panel.master-value": "MASTER: {pct}% — тяните вверх/вниз или крутите колёсико",
    "panel.encoder": "{name} — энкодер: тяните вверх/вниз или крутите колёсико",
    "panel.shortcut": "клавиша {key}",
    "panel.key": "Клавиша {name}",
  },
};

const read = (key) => { try { return localStorage.getItem(key); } catch { return null; } };
const wanted = new URL(location.href).searchParams.get("lang") ?? read("fm1-lang");
let lang = wanted in LANGUAGES ? wanted : "en";
const listeners = [];

export const language = () => lang;

/** The string `key` in the current language, with `{name}` filled in. */
export function t(key, params = {}) {
  const text = STRINGS[lang][key] ?? STRINGS.en[key] ?? key;
  return text.replace(/\{(\w+)\}/g, (match, name) => (name in params ? String(params[name]) : match));
}

/** A catalog field that is either one string or `{ en, ru, … }`. */
export const localized = (value) => (value && typeof value === "object" ? value[lang] ?? value.en : value);

/** Fill in the page's static text (elements marked with data-i18n*). */
export function translatePage(root = document) {
  document.documentElement.lang = lang;
  document.querySelector('meta[name="description"]')?.setAttribute("content", t("meta.description"));
  for (const element of root.querySelectorAll("[data-i18n]")) element.textContent = t(element.dataset.i18n);
  for (const element of root.querySelectorAll("[data-i18n-html]")) element.innerHTML = t(element.dataset.i18nHtml);
  for (const [attribute, data] of [["title", "i18nTitle"], ["placeholder", "i18nPlaceholder"], ["aria-label", "i18nAriaLabel"]]) {
    for (const element of root.querySelectorAll(`[data-${attribute === "aria-label" ? "i18n-aria-label" : `i18n-${attribute}`}]`)) {
      element.setAttribute(attribute, t(element.dataset[data]));
    }
  }
}

/** Switch language, remember it, and redraw everything that listens. */
export function setLanguage(next) {
  if (!(next in LANGUAGES) || next === lang) return;
  lang = next;
  try { localStorage.setItem("fm1-lang", lang); } catch { /* private mode */ }
  translatePage();
  for (const listener of listeners) listener(lang);
}

/** Call `listener(lang)` after every language switch. */
export const onLanguage = (listener) => listeners.push(listener);
