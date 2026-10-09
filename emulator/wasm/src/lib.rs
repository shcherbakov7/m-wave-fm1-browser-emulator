// SPDX-License-Identifier: GPL-3.0-only
//! Plain C-ABI exports for the browser. One emulator instance per module
//! instance; the JavaScript worker owns it and calls these functions only from
//! its own thread. Buffers are exchanged through linear memory.
use fm1_emu::{
    cpu::Cpu,
    firmware::Firmware,
    gpio::{panel_contact, PANEL_CONTROLS, PANEL_ENCODERS},
};
use std::cell::RefCell;

#[cfg(target_arch = "wasm32")]
mod jit;

/// Guest oscillator ticks per second.
const OSCILLATOR_HZ: f64 = 24e6;
/// Guest time (24 MHz oscillator ticks, here 100 ms) a key stays closed after
/// a press, so that even a short click lasts long enough for firmware
/// scanning and debounce however slowly the emulator runs.
const MIN_PRESS_TICKS: u64 = 2_400_000;
const LCD_PIXELS: usize = 240 * 240;
/// Guest time each quadrature phase of a turning encoder lasts (4 ms):
/// longer than any firmware's matrix scan, about 60 detents a second.
const ENCODER_PHASE_TICKS: u64 = 96_000;

/// A turning encoder: detents still to play, the current one's direction,
/// its phase (0 = at rest) and when the next phase starts.
#[derive(Clone, Copy, Default)]
struct Encoder {
    pending: i32,
    direction: i32,
    phase: u8,
    next: u64,
}

struct Machine {
    cpu: Option<Cpu>,
    fault: Option<String>,
    message: Vec<u8>,
    lcd_rgba: Vec<u8>,
    lcd_seen: Option<(u64, bool)>,
    held: [bool; PANEL_CONTROLS],
    release_after: [u64; PANEL_CONTROLS],
    closed: [bool; PANEL_CONTROLS],
    encoders: [Encoder; PANEL_ENCODERS.len()],
    /// MASTER potentiometer, 0..=1023 (kept across firmware loads).
    master: u16,
    /// LED samples: per control, how often it was lit; per matrix column,
    /// how often it was selected; and the brightness last reported.
    led_lit: [u32; PANEL_CONTROLS],
    led_column: [u32; 11],
    leds: [u8; PANEL_CONTROLS],
    #[cfg(target_arch = "wasm32")]
    jit: jit::Jit,
}

impl Machine {
    fn new() -> Self {
        Self {
            cpu: None,
            fault: None,
            message: Vec::new(),
            lcd_rgba: vec![0; LCD_PIXELS * 4],
            lcd_seen: None,
            held: [false; PANEL_CONTROLS],
            release_after: [0; PANEL_CONTROLS],
            closed: [false; PANEL_CONTROLS],
            encoders: [Encoder::default(); PANEL_ENCODERS.len()],
            master: 512,
            led_lit: [0; PANEL_CONTROLS],
            led_column: [0; 11],
            leds: [0; PANEL_CONTROLS],
            #[cfg(target_arch = "wasm32")]
            jit: jit::Jit::new(),
        }
    }

    fn set_message(&mut self, text: &str) -> usize {
        self.message.clear();
        self.message.extend_from_slice(text.as_bytes());
        self.message.len()
    }

    fn load(&mut self, data: Vec<u8>) -> Result<(), String> {
        let firmware = Firmware::from_bytes(data)?;
        let mut cpu = Cpu::new(firmware.bus()?, firmware.entry);
        // Application handoff: the SPL's boot-parameter pointer in r0.
        cpu.r[0] = 0x01c7_fe08;
        let previous = std::mem::replace(self, Self::new());
        #[cfg(target_arch = "wasm32")]
        {
            // Host settings outlive the machine.
            self.jit.enabled = previous.jit.enabled;
            self.jit.verify = previous.jit.verify;
            self.jit.chaining = previous.jit.chaining;
        }
        self.master = previous.master;
        cpu.bus.devices.adc.master = self.master;
        drop(previous);
        self.cpu = Some(cpu);
        Ok(())
    }

    /// Apply held/minimum-duration key state and encoder phases to the
    /// guest matrix, and sample the LEDs.
    fn sync_keys(&mut self) {
        let Some(cpu) = &mut self.cpu else { return };
        let now = cpu.bus.oscillator_ticks;
        for id in 0..PANEL_CONTROLS {
            let closed = self.held[id] || now < self.release_after[id];
            if closed != self.closed[id] {
                if let Some((column, row)) = panel_contact(id) {
                    let _ = cpu.bus.devices.gpio.press(column, row, closed);
                }
                self.closed[id] = closed;
            }
        }
        for (encoder, &(a, b)) in self.encoders.iter_mut().zip(&PANEL_ENCODERS) {
            if now < encoder.next || (encoder.pending == 0 && encoder.phase == 0) {
                continue;
            }
            if encoder.phase == 0 {
                encoder.direction = encoder.pending.signum();
                encoder.pending -= encoder.direction;
            }
            // Clockwise: B closes first, then A; B opens first, then A.
            let (first, second) = if encoder.direction > 0 { (b, a) } else { (a, b) };
            let (contact, closed) = match encoder.phase {
                0 => (first, true),
                1 => (second, true),
                2 => (first, false),
                _ => (second, false),
            };
            let _ = cpu.bus.devices.gpio.press(contact.0, contact.1, closed);
            encoder.phase = (encoder.phase + 1) % 4;
            encoder.next = now + ENCODER_PHASE_TICKS;
        }
        // LEDs: lit while their column is selected and their line is high.
        let gpio = &cpu.bus.devices.gpio;
        let (columns, rows) = (gpio.selected_columns(), gpio.led_rows());
        if columns != 0 {
            for column in 0..11 {
                if columns & (1 << column) != 0 {
                    self.led_column[column] += 1;
                }
            }
            if rows != 0 {
                for id in 0..PANEL_CONTROLS {
                    if let Some((column, row)) = panel_contact(id) {
                        if columns & (1 << column) != 0 && rows & (1 << row) != 0 {
                            self.led_lit[id] += 1;
                        }
                    }
                }
            }
        }
    }

    /// LED brightness (0..=255) per control since the previous call; a
    /// column not seen since keeps its LEDs as they were.
    fn take_leds(&mut self) -> &[u8; PANEL_CONTROLS] {
        for id in 0..PANEL_CONTROLS {
            let Some((column, _)) = panel_contact(id) else { continue };
            let seen = self.led_column[column];
            if seen != 0 {
                self.leds[id] = (self.led_lit[id].min(seen) * 255 / seen) as u8;
            }
            self.led_lit[id] = 0;
        }
        self.led_column = [0; 11];
        &self.leds
    }

    fn run(&mut self, steps: u32) -> i32 {
        self.run_until(u64::MAX, steps, 1 << 20)
    }

    /// Run up to `steps` steps, stopping early once guest time reaches
    /// `target` oscillator ticks (checked every `chunk` steps).
    fn run_until(&mut self, target: u64, steps: u32, chunk: u32) -> i32 {
        if self.fault.is_some() {
            return 1;
        }
        if self.cpu.is_none() {
            return 2;
        }
        // Re-check key release timing at a bounded granularity.
        let mut remaining = steps;
        while remaining > 0 {
            let cpu = self.cpu.as_mut().unwrap();
            if cpu.bus.oscillator_ticks >= target {
                break;
            }
            let chunk = remaining.min(chunk);
            #[cfg(target_arch = "wasm32")]
            let result = self.jit.run(cpu, chunk as u64);
            #[cfg(not(target_arch = "wasm32"))]
            let result = cpu
                .step_many(chunk as u64)
                .map_err(|error| format!("{error} (after {} steps)", cpu.steps));
            if let Err(error) = result {
                self.fault = Some(error);
                return 1;
            }
            remaining -= chunk;
            self.sync_keys();
        }
        0
    }

    /// Convert the LCD to RGBA when it changed since the previous call.
    fn lcd(&mut self) -> bool {
        let Some(cpu) = &self.cpu else { return false };
        let visible = cpu.bus.screen_visible();
        let state = (cpu.bus.lcd.pixels_written, visible);
        if self.lcd_seen == Some(state) {
            return false;
        }
        self.lcd_seen = Some(state);
        for (rgba, &rgb) in self.lcd_rgba.chunks_exact_mut(4).zip(&cpu.bus.lcd.pixels) {
            let rgb = if visible { rgb } else { 0 };
            rgba[0] = (rgb >> 16) as u8;
            rgba[1] = (rgb >> 8) as u8;
            rgba[2] = rgb as u8;
            rgba[3] = 255;
        }
        true
    }
}

thread_local! {
    static MACHINE: RefCell<Machine> = RefCell::new(Machine::new());
}

fn with<T>(f: impl FnOnce(&mut Machine) -> T) -> T {
    MACHINE.with(|machine| f(&mut machine.borrow_mut()))
}

/// Allocate `len` bytes for the host to fill.
#[no_mangle]
pub extern "C" fn fm1_alloc(len: usize) -> *mut u8 {
    let mut buffer = Vec::<u8>::with_capacity(len.max(1));
    let ptr = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    ptr
}

/// Free a buffer from `fm1_alloc` that was not passed to `fm1_load`.
///
/// # Safety
/// `ptr`/`len` must come from one `fm1_alloc(len)` call.
#[no_mangle]
pub unsafe extern "C" fn fm1_free(ptr: *mut u8, len: usize) {
    drop(Vec::from_raw_parts(ptr, 0, len.max(1)));
}

/// Load firmware from an `fm1_alloc` buffer (ownership passes here).
/// Returns 0 on success; otherwise the error is in the message buffer.
///
/// # Safety
/// `ptr`/`len` must come from one `fm1_alloc(len)` call, fully initialized.
#[no_mangle]
pub unsafe extern "C" fn fm1_load(ptr: *mut u8, len: usize) -> i32 {
    let data = Vec::from_raw_parts(ptr, len, len.max(1));
    with(|m| match m.load(data) {
        Ok(()) => 0,
        Err(error) => {
            m.set_message(&error);
            1
        }
    })
}

/// Execute up to `steps` guest steps. 0 = ran, 1 = fault (see message),
/// 2 = nothing loaded.
#[no_mangle]
pub extern "C" fn fm1_run(steps: u32) -> i32 {
    with(|m| {
        let status = m.run(steps);
        if status == 1 {
            let fault = m.fault.clone().unwrap_or_default();
            m.set_message(&fault);
        }
        status
    })
}

/// Run until guest time reaches `seconds` or `max_steps` steps have run,
/// whichever comes first. Hosts pace emulation against their clock with it.
/// Returns as `fm1_run`.
#[no_mangle]
pub extern "C" fn fm1_run_until(seconds: f64, max_steps: u32) -> i32 {
    with(|m| {
        let target = (seconds.max(0.0) * OSCILLATOR_HZ) as u64;
        let status = m.run_until(target, max_steps, 8192);
        if status == 1 {
            let fault = m.fault.clone().unwrap_or_default();
            m.set_message(&fault);
        }
        status
    })
}

/// Guest time in seconds since boot (0 when nothing is loaded).
#[no_mangle]
pub extern "C" fn fm1_guest_seconds() -> f64 {
    with(|m| m.cpu.as_ref().map_or(0.0, |cpu| cpu.bus.oscillator_ticks as f64 / OSCILLATOR_HZ))
}

/// Guest steps executed since boot.
#[no_mangle]
pub extern "C" fn fm1_steps() -> f64 {
    with(|m| m.cpu.as_ref().map_or(0.0, |cpu| cpu.steps as f64))
}

/// Turn rotary encoder `index` (SELECT, PRESETS, ALGORITHM, KNOB 1-4) by
/// `steps` detents, positive clockwise. The detents play out over guest time.
#[no_mangle]
pub extern "C" fn fm1_encoder(index: u32, steps: i32) {
    with(|m| {
        if let Some(encoder) = m.encoders.get_mut(index as usize) {
            encoder.pending = (encoder.pending + steps).clamp(-32, 32);
        }
    })
}

/// Set the MASTER potentiometer (0..=1023).
#[no_mangle]
pub extern "C" fn fm1_master(value: u32) {
    with(|m| {
        m.master = value.min(1023) as u16;
        if let Some(cpu) = &mut m.cpu {
            cpu.bus.devices.adc.master = m.master;
        }
    })
}

/// Pointer to the LED brightness of the 41 panel controls (bytes, 0..=255,
/// by control ID), measured since the previous call.
#[no_mangle]
pub extern "C" fn fm1_leds() -> *const u8 {
    with(|m| m.take_leds().as_ptr())
}

/// Press (1) or release (0) a panel control (see `PANEL_KEYMAP`).
#[no_mangle]
pub extern "C" fn fm1_key(id: u32, down: u32) {
    with(|m| {
        let id = id as usize;
        if id >= PANEL_CONTROLS {
            return;
        }
        m.held[id] = down != 0;
        if down != 0 {
            let now = m.cpu.as_ref().map_or(0, |cpu| cpu.bus.oscillator_ticks);
            m.release_after[id] = now + MIN_PRESS_TICKS;
        }
        m.sync_keys();
    })
}

/// Pointer to the 240×240 RGBA frame if it changed since the last call, else 0.
#[no_mangle]
pub extern "C" fn fm1_lcd() -> *const u8 {
    with(|m| {
        if m.lcd() {
            m.lcd_rgba.as_ptr()
        } else {
            std::ptr::null()
        }
    })
}

/// Move up to `max_frames` stereo frames of guest audio into `out` as
/// interleaved f32 in [-1, 1]. Returns the number of frames written.
///
/// # Safety
/// `out` must hold `2 * max_frames` f32 values.
#[no_mangle]
pub unsafe extern "C" fn fm1_audio(out: *mut f32, max_frames: usize) -> usize {
    with(|m| {
        let Some(cpu) = &mut m.cpu else { return 0 };
        let out = std::slice::from_raw_parts_mut(out, max_frames * 2);
        let samples = &mut cpu.bus.audio.samples;
        let count = samples.len().min(max_frames);
        for (frame, [left, right]) in out.chunks_exact_mut(2).zip(samples.drain(..count)) {
            // ALNK words carry 24-bit samples in the low bits.
            frame[0] = (left << 8 >> 8) as f32 / 8_388_608.0;
            frame[1] = (right << 8 >> 8) as f32 / 8_388_608.0;
        }
        count
    })
}

/// Move up to `max` bytes of USB CDC console output into `out`.
///
/// # Safety
/// `out` must hold `max` bytes.
#[no_mangle]
pub unsafe extern "C" fn fm1_serial(out: *mut u8, max: usize) -> usize {
    with(|m| {
        let Some(cpu) = &mut m.cpu else { return 0 };
        let out = std::slice::from_raw_parts_mut(out, max);
        let count = cpu.bus.usb.serial.len().min(max);
        for (slot, byte) in out.iter_mut().zip(cpu.bus.usb.serial.drain(..count)) {
            *slot = byte;
        }
        count
    })
}

/// Offer console input to the guest's USB CDC OUT endpoint. Returns 1 when
/// accepted; 0 means retry later.
///
/// # Safety
/// `ptr` must hold `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn fm1_serial_in(ptr: *const u8, len: usize) -> i32 {
    let bytes = std::slice::from_raw_parts(ptr, len);
    with(|m| match &mut m.cpu {
        Some(cpu) => cpu.bus.usb.receive_serial(bytes) as i32,
        None => 0,
    })
}

/// Write a JSON status object into the message buffer; returns its length.
#[no_mangle]
pub extern "C" fn fm1_status() -> usize {
    with(|m| {
        let text = match &m.cpu {
            None => "{\"loaded\":false}".to_string(),
            Some(cpu) => {
                #[cfg(target_arch = "wasm32")]
                let jit = format!(
                    "{{\"enabled\":{},\"chaining\":{},\"compiled\":{},\"failed\":{},\"translatedSteps\":{},\
                     \"blockCalls\":{},\"execCalls\":{},\"batchedSteps\":{},\"flushes\":{}}}",
                    m.jit.enabled,
                    m.jit.chaining,
                    m.jit.stats.compiled,
                    m.jit.stats.failed,
                    m.jit.stats.translated_steps,
                    m.jit.stats.block_calls,
                    // SAFETY: plain read on the single host thread.
                    unsafe { *std::ptr::addr_of!(jit::EXEC_CALLS) },
                    cpu.batched_steps,
                    m.jit.stats.flushes
                );
                #[cfg(not(target_arch = "wasm32"))]
                let jit = "null".to_string();
                format!(
                "{{\"loaded\":true,\"steps\":{},\"guestSeconds\":{},\"irqs\":{},\"lcdPixels\":{},\
                 \"visible\":{},\"audioFrames\":{},\"watchdogFeeds\":{},\"jit\":{},\"fault\":{}}}",
                cpu.steps,
                cpu.bus.oscillator_ticks as f64 / OSCILLATOR_HZ,
                cpu.irq_entries,
                cpu.bus.lcd.pixels_written,
                cpu.bus.screen_visible(),
                cpu.bus.audio.frames,
                cpu.bus.system.watchdog_feeds,
                jit,
                m.fault
                    .as_ref()
                    .map_or("null".to_string(), |f| format!("{f:?}")),
                )
            }
        };
        m.set_message(&text)
    })
}

#[no_mangle]
pub extern "C" fn fm1_message_ptr() -> *const u8 {
    with(|m| m.message.as_ptr())
}

#[no_mangle]
pub extern "C" fn fm1_message_len() -> usize {
    with(|m| m.message.len())
}

/// Turn block translation on (1) or off (0) for the loaded machine.
#[no_mangle]
pub extern "C" fn fm1_set_jit(enabled: u32) {
    with(|_m| {
        #[cfg(target_arch = "wasm32")]
        {
            _m.jit.enabled = enabled != 0;
        }
        let _ = enabled;
    })
}

/// Start (1) or stop and report (0) counting interpreter calls from
/// translated blocks; the report goes to the message buffer.
#[no_mangle]
pub extern "C" fn fm1_profile_exec(start: u32) -> usize {
    #[cfg(target_arch = "wasm32")]
    // SAFETY: single host thread; not called while a block runs.
    unsafe {
        let ops = &mut *std::ptr::addr_of_mut!(jit::EXEC_OPS);
        if start != 0 {
            *ops = Some(Default::default());
            return 0;
        }
        let by_pc = ops.take().unwrap_or_default();
        let mut by_name: std::collections::HashMap<&str, u64> = Default::default();
        for ((name, _), count) in &by_pc {
            *by_name.entry(name).or_default() += count;
        }
        let mut list: Vec<_> = by_name.into_iter().collect();
        list.sort_by(|a, b| b.1.cmp(&a.1));
        let mut text: Vec<String> = list.iter().take(30).map(|(n, c)| format!("{n} {c}")).collect();
        let mut pcs: Vec<_> = by_pc.into_iter().collect();
        pcs.sort_by(|a, b| b.1.cmp(&a.1));
        text.extend(
            pcs.iter()
                .take(30)
                .map(|((name, pc), count)| format!("pc {pc:08x} {name} {count}")),
        );
        return with(|m| m.set_message(&text.join("\n")));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = start;
        0
    }
}

/// Turn per-block verification against the interpreter on (1) or off (0).
/// Let translated blocks call each other directly (the host must support
/// WebAssembly tail calls). Takes effect for blocks translated afterwards.
#[no_mangle]
pub extern "C" fn fm1_set_jit_chaining(enabled: u32) {
    #[cfg(target_arch = "wasm32")]
    with(|m| m.jit.chaining = enabled != 0);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = enabled;
}

#[no_mangle]
pub extern "C" fn fm1_set_jit_verify(enabled: u32) {
    with(|_m| {
        #[cfg(target_arch = "wasm32")]
        {
            _m.jit.verify = enabled != 0;
        }
        let _ = enabled;
    })
}
