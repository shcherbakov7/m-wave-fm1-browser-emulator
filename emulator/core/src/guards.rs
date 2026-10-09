// SPDX-License-Identifier: GPL-3.0-only
use std::collections::BTreeMap;
#[derive(Default)]
pub struct Guards {
    registers: BTreeMap<u32, u32>,
    /// Enabled write-protection windows `(low, high)` from DBG_EN/WR_LIMIT,
    /// refreshed on every guard write so `check_write` needs no map lookups.
    windows: Vec<(u32, u32)>,
}
impl Guards {
    fn known(a: u32) -> bool {
        matches!(
            a,
            0x1eee240
                | 0x1eee244
                | 0x1eee248
                | 0x1eee340
                | 0x1eee348
                | 0x1eef0d0
                | 0x1eef0d4
                | 0x1eef0d8
                | 0x1eef0dc
                | 0x1eef0e0
                | 0x1eef0e4
                | 0x1eef1c0
                | 0x1eef3c0
                | 0x1eef1c4
                | 0x1eef1c8
                | 0x1eef1cc
                | 0x1eef1d0
        ) || (0x1eee280..0x1eee28c).contains(&a)
            || (0x1eee2c0..0x1eee2cc).contains(&a)
            || (0x1eee380..0x1eee390).contains(&a)
            || (0x1eee340..=0x1eee358).contains(&a)
            || (0x1eef2d0..=0x1eef2e4).contains(&a)
            || (0x41c00..=0x41c18).contains(&a)
            // corex2 performance counters (SDK csfr.h C0_*_UACNT/TL_CKCNT).
            // Not modeled: they read as stopped, so X0X falls back to TIMER4.
            || (0x1eee200..0x1eee220).contains(&a)
    }
    fn value(&self, a: u32) -> u32 {
        *self.registers.get(&a).unwrap_or(&0)
    }
    pub fn read(&self, a: u32) -> Option<u32> {
        Self::known(a).then(|| self.value(a))
    }
    pub fn write(&mut self, a: u32, v: u32) -> Option<Result<(), &'static str>> {
        if !Self::known(a) {
            return None;
        }
        match a {
            0x1eee240 => {
                if v == 0xe7 {
                    self.registers.insert(a, self.value(a) ^ 1);
                }
            }
            0x1eee248 => {
                self.registers.insert(0x1eee244, self.value(0x1eee244) & !v);
            }
            0x1eef0d4 | 0x1eef2d4 => {
                self.registers.insert(a, self.value(a) & !v);
            }
            _ => {
                self.registers.insert(a, v);
            }
        }
        self.windows = (0..3)
            .filter(|n| self.value(0x1eee348) & (1 << n) != 0)
            .map(|n| (self.value(0x1eee2c0 + n * 4), self.value(0x1eee280 + n * 4)))
            .collect();
        Some(Ok(()))
    }
    pub(crate) fn active(&self) -> bool {
        !self.windows.is_empty()
    }
    pub fn check_write(&self, a: u32, size: usize) -> Result<(), &'static str> {
        for &(low, high) in &self.windows {
            if a <= high && a as u64 + size as u64 > low as u64 {
                return Err("CPU write protection violation");
            }
        }
        Ok(())
    }
}
