//! GPIO matrix: output/enable/input words, per-pin config and interrupt types, the function
//! selection tables, and the edge queues the SoC hands to its board and pulse counters.
use crate::device::{Device, WriteEffect};
use crate::regram::RegRam;

// ------------------------------------------------------------------ GPIO
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Gpio {
    pub out: u64, pub enable: u64, pub input: u64, pub status: u64,
    #[serde(with = "emu_core::snap::arr")]
    pub pin: [u32; 49],
    #[serde(with = "emu_core::snap::arr")]
    pub func_in_sel: [u32; 256],
    #[serde(with = "emu_core::snap::arr")]
    pub func_out_sel: [u32; 49],
    ram: RegRam,
    pub input_changes: Vec<(u8, bool)>,
    /// (pin, level) changes of enabled outputs since last drain
    pub changes: Vec<(u8, bool)>,
    pub strap: u32,
}
impl Gpio {
    /// A pin armed as a light-sleep wake source (`gpio_wakeup_enable`: PINn.WAKEUP_ENABLE with a
    /// low or high level interrupt type) currently sits at that level (NOTE fork).
    pub fn wakeup_active(&self) -> bool {
        (0..49).any(|n| {
            let reg = self.pin[n];
            let level = (self.input >> n) & 1 != 0;
            reg & (1 << 10) != 0 && matches!(((reg >> 7) & 7, level), (4, false) | (5, true))
        })
    }

    pub fn new() -> Self { Gpio { out: 0, enable: 0, input: (1u64 << 49) - 1, status: 0, pin: [0; 49], func_in_sel: [0x3c; 256], func_out_sel: [0x100; 49], ram: RegRam::new(), changes: Vec::new(), strap: 0x0f, input_changes: Vec::new() } }
    /// Report every pin whose driven level changed: `out & enable` before against after. Both
    /// words matter — a driver that toggles the output *enable* to produce a level (IDF 5.5's
    /// esp_lcd releases the D/C line after each colour transfer and re-enables it before the
    /// next) changes what a board sees just as much as one that toggles `out`.
    fn note_out(&mut self, old_out: u64, old_enable: u64) {
        let vis = self.out & self.enable; let oldvis = old_out & old_enable;
        let mut diff = (vis ^ oldvis) & ((1u64 << 49) - 1);
        while diff != 0 {
            let p = diff.trailing_zeros() as u8;
            self.changes.push((p, vis & (1u64 << p) != 0));
            diff &= diff - 1;
        }
    }
    pub fn set_input(&mut self, pin: u8, level: bool) -> bool {
        let Some(&cfg) = self.pin.get(pin as usize) else { return false; };
        let old = self.input;
        if level { self.input |= 1u64 << pin; } else { self.input &= !(1u64 << pin); }
        if old == self.input { return false; }
        self.input_changes.push((pin, level));
        // edge detection per GPIO_PINn INT_TYPE (bits 7..9): 1 rising, 2 falling, 3 any, 4 low level, 5 high level
        let typ = (cfg >> 7) & 7;
        let rising = level && (typ == 1 || typ == 3);
        let falling = !level && (typ == 2 || typ == 3);
        if rising || falling { self.status |= 1u64 << pin; return true; }
        false
    }
    /// The pin the matrix routes peripheral output signal `sig` to, if any.
    pub fn pin_for_signal(&self, sig: u32) -> Option<u8> {
        self.func_out_sel.iter().position(|&s| s & 0x1ff == sig).map(|p| p as u8)
    }
    pub fn level(&self, pin: u8) -> bool {
        if pin as usize >= self.pin.len() { return false; }
        if self.enable & (1u64 << pin) != 0 { self.out & (1u64 << pin) != 0 } else { self.input & (1u64 << pin) != 0 }
    }
    /// Pins whose level-type interrupt condition (INT_TYPE 4 low, 5 high) holds right now. On
    /// silicon these show in STATUS/PCPU_INT for as long as the level lasts: a W1TC write cannot
    /// clear them. The IDF ISR service finds its pin through those registers (NOTE fork).
    fn level_status(&self) -> u64 {
        let mut m = 0u64;
        for p in 0..49u8 {
            let typ = (self.pin[p as usize] >> 7) & 7;
            if (typ == 4 && !self.level(p)) || (typ == 5 && self.level(p)) { m |= 1u64 << p; }
        }
        m
    }
    /// STATUS as software reads it: latched edges plus active levels.
    fn status_view(&self) -> u64 { self.status | self.level_status() }
    /// Interrupt status routed to core 0 (PCPU_INT): pins with INT_ENA bit 13.
    fn pcpu_status(&self) -> u64 {
        let v = self.status_view();
        (0..49u8).filter(|&p| v & (1u64 << p) != 0 && (self.pin[p as usize] >> 13) & 1 != 0).fold(0, |m, p| m | (1u64 << p))
    }
    pub fn irq(&self) -> bool {
        // level-type interrupts on current input, plus latched edge status, gated by INT_ENA (bits 13..17, bit 13 = core0)
        (0..49u8).any(|p| { let cfg = self.pin[p as usize]; let ena = (cfg >> 13) & 1 != 0; let typ = (cfg >> 7) & 7;
            ena && ((self.status & (1u64 << p) != 0) || (typ == 4 && !self.level(p)) || (typ == 5 && self.level(p))) })
    }
    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x4 => self.out as u32, 0x10 => (self.out >> 32) as u32,
            0x20 => self.enable as u32, 0x2c => (self.enable >> 32) as u32,
            0x38 => self.strap,
            0x3c => self.input as u32, 0x40 => (self.input >> 32) as u32,
            0x44 => self.status_view() as u32, 0x50 => (self.status_view() >> 32) as u32,
            0x5c => self.pcpu_status() as u32, 0x68 => (self.pcpu_status() >> 32) as u32,     // PCPU_INT: interrupt status seen by core 0
            0x74..=0x134 => self.pin[((off - 0x74) / 4) as usize],
            0x154..=0x550 => self.func_in_sel[((off - 0x154) / 4) as usize],
            0x554..=0x614 => self.func_out_sel[((off - 0x554) / 4) as usize],
            0x6fc => 0x2006130,
            _ => self.ram.read(off),
        }
    }
    pub fn write(&mut self, off: u32, v: u32) {
        let (old, old_enable) = (self.out, self.enable);
        match off {
            0x4 => self.out = (self.out & !0xffff_ffff) | v as u64,
            0x8 => self.out |= v as u64,
            0xc => self.out &= !(v as u64),
            0x10 => self.out = (self.out & 0xffff_ffff) | ((v as u64 & 0x1ffff) << 32),
            0x14 => self.out |= (v as u64) << 32,
            0x18 => self.out &= !((v as u64) << 32),
            0x20 => { self.enable = (self.enable & !0xffff_ffff) | v as u64; }
            0x24 => self.enable |= v as u64,
            0x28 => self.enable &= !(v as u64),
            0x2c => self.enable = (self.enable & 0xffff_ffff) | ((v as u64 & 0x1ffff) << 32),
            0x30 => self.enable |= (v as u64) << 32,
            0x34 => self.enable &= !((v as u64) << 32),
            0x44 => self.status = (self.status & !0xffff_ffff) | v as u64,
            0x48 => self.status |= v as u64,
            0x4c => self.status &= !(v as u64),
            0x50 => self.status = (self.status & 0xffff_ffff) | ((v as u64) << 32),
            0x54 => self.status |= (v as u64) << 32,
            0x58 => self.status &= !((v as u64) << 32),
            0x74..=0x134 => self.pin[((off - 0x74) / 4) as usize] = v,
            0x154..=0x550 => self.func_in_sel[((off - 0x154) / 4) as usize] = v,
            0x554..=0x614 => self.func_out_sel[((off - 0x554) / 4) as usize] = v,
            _ => self.ram.write(off, v),
        }
        // enable changes also change what's visible on pins
        if matches!(off, 0x4 | 0x8 | 0xc | 0x10 | 0x14 | 0x18 | 0x20 | 0x24 | 0x28 | 0x2c | 0x30 | 0x34) { self.note_out(old, old_enable); }
    }
}

impl Default for Gpio { fn default() -> Self { Self::new() } }

impl Device for Gpio {
    fn read(&mut self, off: u32) -> u32 { Gpio::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { Gpio::write(self, off, v); WriteEffect::NONE }
    fn irq_sources(&self) -> u64 { self.irq() as u64 }
}

#[cfg(test)]
mod level_tests {
    use super::*;

    #[test]
    fn a_held_low_level_interrupt_is_visible_in_pcpu_int_until_released() {
        let mut g = Gpio::new();
        g.write(0x74, (4 << 7) | (1 << 13));             // PIN0: low level, core-0 enable
        assert_eq!(g.read(0x5c) & 1, 0, "idle high");
        g.set_input(0, false);
        assert!(g.irq());
        assert_eq!(g.read(0x5c) & 1, 1, "the ISR service can find the pin");
        g.write(0x4c, 1);                                 // W1TC does not clear an active level
        assert_eq!(g.read(0x44) & 1, 1);
        g.set_input(0, true);
        assert_eq!((g.read(0x5c) & 1, g.irq()), (0, false));
        g.write(0x74, (4 << 7) | (1 << 10));              // wake-only (no INT_ENA): STATUS yes, PCPU_INT no
        g.set_input(0, false);
        assert_eq!((g.read(0x44) & 1, g.read(0x5c) & 1), (1, 0));
    }
}
