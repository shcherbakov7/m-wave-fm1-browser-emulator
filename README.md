# M-VAVE FM-1 — эмулятор в браузере

Эмулятор синтезатора **M-VAVE FM-1**, который запускает **немодифицированные
прошивки** (`.fwsc`, `.ufw`, а также `.bin`/`.elf`) прямо в браузере. Он
эмулирует процессор JieLi AC791N (набор инструкций pi32v2, два ядра) и
периферию платы: экран 240×240, клавиатуру из 27 клавиш и 14 кнопок, аудио,
USB, флеш-память. Ядро написано на Rust и собирается в WebAssembly.

![Восемь прошивок в эмуляторе](docs/compatibility.png)

## Совместимость

Проверено на 300 млн инструкций без ошибок, с выводом интерфейса на экран:

| Прошивка | Статус |
|---|---|
| Официальная V13, V14, V15 (`FM-1.fwsc`) | загружается, экран, звук, кнопки |
| Felucca 1.1.5.1 | загружается, интерфейс, звук, USB-консоль, кнопки |
| SLOOP 2.4.1 | загружается до заставки и далее |
| SLOOP ALG-02 (ALG05-TEST) | загружается до заставки и далее |
| X0X 0.10.3-beta | загружается, интерфейс секвенсора |
| Melodee 0.13.1 | загружается до заставки и далее |

Не проверялись (нет файлов): Baud Girl FM-1+VA, Groove OS (платная),
Jangada, ChoralRoot, FiMba и другие. Если прошивка остановится, эмулятор
покажет адрес и причину. Пришлите их в issue.

## Как пользоваться

1. Откройте страницу эмулятора (GitHub Pages, см. ниже) или запустите локально.
2. Нажмите **«Загрузить прошивку»** или перетащите файл `.fwsc` на панель.
   Прошивки не входят в комплект. Официальную можно скачать на
   [m-vave.com](https://www.m-vave.com/download), сторонние — на страницах
   их авторов ([FM-1 Firmware Hub](https://fm1.designburgapps.com/)).
3. Кнопки и клавиши нажимаются мышью или касанием. Клавиатура компьютера:
   `Z`…`/` и `S D F H J L ;` — нижняя часть клавиатуры, `1 Q…Y 3 4 6` —
   верхняя, `←`/`→` — OCT−/OCT+, `Esc` — HOME.
4. Кнопка **«Звук»** включает вывод звука. Загруженные прошивки запоминаются
   в браузере (список «Недавние»).

## Ограничения

- **Скорость.** Пока эмуляция идёт в несколько процентов от скорости
  настоящего устройства (Felucca в Chromium — около 2%). Интерфейс работает,
  но медленно, звук прерывистый. Ускорение — главная задача сейчас.
- Ручки (энкодеры) пока не эмулируются.
- USB MIDI с компьютера, Bluetooth и установка прошивки «по воздуху»
  (через эмулированный апдейтер) не поддерживаются.
- Для неподтверждённых на железе случаев (деление на ноль во float и т.п.)
  используется стандартное поведение IEEE 754.

## Сборка и запуск локально

Нужны Rust (с целью `wasm32-unknown-unknown`) и Python или Node.js для
статического сервера:

```sh
rustup target add wasm32-unknown-unknown
tools/build-web.sh                  # собирает web/fm1.wasm
python3 -m http.server -d web 8080  # затем откройте http://localhost:8080
```

Тесты и инструменты:

```sh
cargo test --release --manifest-path emulator/Cargo.toml
# Нативная диагностика прошивки: где остановилась, что на экране
cargo run --release --manifest-path emulator/Cargo.toml --example diagnose -- FIRMWARE.fwsc 300000000
FM1_LCD_PPM=screen.ppm cargo run --release ... --example diagnose -- FIRMWARE.fwsc 300000000
# Скорость и состав инструкций
PROFILE_OPS=1 cargo run --release --manifest-path emulator/Cargo.toml --example profile -- FIRMWARE.fwsc
# Проверка в настоящем Chromium (npm install для Playwright)
node tools/browser-smoke.mjs FIRMWARE.fwsc screenshot.png
```

## Публикация на GitHub Pages

Workflow `.github/workflows/ci.yml` прогоняет тесты, собирает WebAssembly и
публикует сайт из ветки по умолчанию. Один раз включите в настройках
репозитория: **Settings → Pages → Source: GitHub Actions**.

## Устройство

```
web/                     страница, Web Worker с эмулятором, AudioWorklet
emulator/core/           ядро: CPU pi32v2, шина, периферия FM-1, загрузчик .fwsc
emulator/wasm/           C-ABI обёртка ядра для браузера
emulator/fixtures/       маленькие тестовые прошивки из upstream
tools/                   сборка, проверки в Node и Chromium
```

Ядро работает в Web Worker отрезками по ~12 мс. Между ними обрабатываются
нажатия, отправляются кадры экрана (до 30 к/с) и звук. Пока единственное
работающее ядро крутится в цикле ожидания таймера или простаивает до
прерывания, время устройства продвигается быстрее («time warp»).

## Лицензия и благодарности

GPL-3.0-only, см. [LICENSE](LICENSE) и [LICENSES.md](LICENSES.md).

Ядро основано на Rust-эмуляторе из
[simonjohansson/fm1-emulator](https://github.com/simonjohansson/fm1-emulator)
(GPL-3.0). Новые инструкции и исправления сверялись с дизассемблером
производителя из [AL-255/FM-1-RE](https://github.com/AL-255/FM-1-RE),
SLEIGH-описанием [quarkslab/ghidra-jieli](https://github.com/quarkslab/ghidra-jieli)
и исходниками [Felucca](https://github.com/hugelton/Felucca),
[SLOOP](https://github.com/isod89/sloop-fm1) и
[X0X](https://github.com/charlesvestal/fm1-x0x).
