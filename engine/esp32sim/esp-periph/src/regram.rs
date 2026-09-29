use crate::device::{Device, WriteEffect};

/// Generic 4 KiB register block backed by RAM: what a device we only need to "accept" is, and the
/// fallback behind every modelled device's unhandled offsets.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct RegRam { pub regs: Vec<u32> }
impl RegRam {
    pub fn new() -> Self { RegRam { regs: vec![0; 1024] } }
    pub fn read(&self, off: u32) -> u32 { self.regs[((off & 0xfff) >> 2) as usize] }
    pub fn write(&mut self, off: u32, v: u32) { self.regs[((off & 0xfff) >> 2) as usize] = v; }
}
impl Default for RegRam { fn default() -> Self { Self::new() } }
impl Device for RegRam {
    fn read(&mut self, off: u32) -> u32 { RegRam::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { RegRam::write(self, off, v); WriteEffect::NONE }
}
