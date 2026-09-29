use crate::device::{Device, WriteEffect};
use crate::regram::RegRam;

// ------------------------------------------------------------------ SYSTEM
/// SYSTEM registers, including the `SYSTEM_CPU_INTR_FROM_CPU_0..3` software-interrupt latches
/// (at `sw_int_off`: 0x30 on the S3, 0x28 on the C3), which are interrupt sources.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SystemRegs { pub ram: RegRam, pub sw_int: u32, sw_int_off: u32 }
impl SystemRegs {
    /// values the 2nd-stage bootloader leaves behind; used by the HLE app-boot shortcut
    pub fn preset_after_bootloader(&mut self) { self.ram.write(0x10, (1 << 2) | 2); self.ram.write(0x60, 1 << 10); }
    pub fn new(sw_int_off: u32) -> Self {
        let mut s = SystemRegs { ram: RegRam::new(), sw_int: 0, sw_int_off };
        s.ram.write(0x60, 0x000a_8001);        // SYSCLK_CONF reset value (XTAL 40 MHz selected) as read on silicon
        s.ram.write(0x18, 0xffff_ffff); s.ram.write(0x1c, 0xffff_ffff);
        s
    }
    fn sw_bit(&self, off: u32) -> Option<u32> { if (self.sw_int_off..self.sw_int_off + 16).contains(&off) { Some((off - self.sw_int_off) / 4) } else { None } }
    pub fn read(&self, off: u32) -> u32 {
        if let Some(b) = self.sw_bit(off) { return (self.sw_int >> b) & 1; }
        match off { 0xffc => 0x2101220, _ => self.ram.read(off) }
    }
    pub fn write(&mut self, off: u32, v: u32) {
        if let Some(b) = self.sw_bit(off) { if v & 1 != 0 { self.sw_int |= 1 << b } else { self.sw_int &= !(1 << b) } }
        self.ram.write(off, v);
    }
}

impl Device for SystemRegs {
    fn read(&mut self, off: u32) -> u32 { SystemRegs::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { SystemRegs::write(self, off, v); WriteEffect::NONE }
    fn irq_sources(&self) -> u64 { (self.sw_int & 0xf) as u64 }
}
