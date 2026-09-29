//! `--vcd FILE`: GPIO edges and interrupt lines as a waveform, one picosecond per unit, for
//! GTKWave, PulseView or a Python parser.
use crate::observe::{Ctx, Observer, Wants};
use crate::soc::Soc;
use emu_core::Trap;
use std::io::Write;

pub struct Vcd { path: String, cpu_hz: u64, events: Vec<(u64, u16, bool)>, seen: [bool; 256] }
impl Vcd {
    pub fn new(path: &str, cpu_hz: u64) -> Self { Vcd { path: path.to_string(), cpu_hz, events: Vec::new(), seen: [false; 256] } }
    fn ps(&self, cycles: u64) -> u64 { (cycles as u128 * 1_000_000_000_000u128 / self.cpu_hz as u128) as u64 }
    /// signal ids: 0..=127 GPIO pins, 128 + core*32 + line: interrupt line raised/taken
    fn push(&mut self, cycle: u64, id: u16, level: bool) { if (id as usize) < 256 { self.seen[id as usize] = true; } self.events.push((cycle, id, level)); }
}
impl<S: Soc> Observer<S> for Vcd {
    fn name(&self) -> &'static str { "vcd" }
    fn wants(&self) -> Wants { Wants::GPIO | Wants::IRQ | Wants::TRAP }
    fn on_gpio(&mut self, cycle: u64, pin: u8, level: bool) { self.push(cycle, pin as u16, level); }
    fn on_irq_raised(&mut self, cx: &Ctx, core: usize, line: u32) { self.push(cx.cycles, 128 + (core as u16) * 32 + (line & 31) as u16, true); }
    fn on_trap(&mut self, cx: &Ctx, core: usize, _cpu: &S::Core, _pc: u32, trap: &Trap) {
        if let Trap::Interrupt(line) = trap { self.push(cx.cycles, 128 + (core as u16) * 32 + (line & 31) as u16, false); }
    }
    fn report(&mut self, _cx: &Ctx) -> String {
        let Ok(f) = std::fs::File::create(&self.path) else { return format!("[vcd] cannot write {}", self.path) };
        let mut f = std::io::BufWriter::new(f);
        let code = |id: u16| -> String { format!("s{}", id) };
        let _ = writeln!(f, "$timescale 1ps $end\n$scope module esp32sim $end");
        for id in 0..256u16 {
            if !self.seen[id as usize] { continue; }
            let name = if id < 128 { format!("gpio{}", id) } else { format!("core{}_int{}", (id - 128) / 32, (id - 128) % 32) };
            let _ = writeln!(f, "$var wire 1 {} {} $end", code(id), name);
        }
        let _ = writeln!(f, "$upscope $end\n$enddefinitions $end\n#0");
        for id in 0..256u16 { if self.seen[id as usize] { let _ = writeln!(f, "{}{}", if id < 128 { 'x' } else { '0' }, code(id)); } }
        self.events.sort_by_key(|e| e.0);
        let mut last = u64::MAX;
        for &(cycle, id, level) in &self.events {
            let t = self.ps(cycle);
            if t != last { let _ = writeln!(f, "#{}", t); last = t; }
            let _ = writeln!(f, "{}{}", level as u8, code(id));
        }
        format!("[vcd] wrote {} events to {}", self.events.len(), self.path)
    }
}
