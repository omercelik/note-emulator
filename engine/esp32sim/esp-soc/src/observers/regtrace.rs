//! `--regtrace FILE`: the compact per-instruction register trace `hw/compare.py` diffs against a
//! real chip single-stepped over JTAG.
use crate::observe::{Ctx, Observer, Wants};
use crate::soc::{Soc, Stop};
use emu_core::Core;
use std::io::Write;

pub struct RegTrace { out: std::io::BufWriter<std::fs::File>, pub core: usize, pub max: u64, pub from_pc: Option<u32>, armed: bool, count: u64 }
impl RegTrace {
    pub fn new(out: std::fs::File, max: u64, from_pc: Option<u32>) -> Self { RegTrace { out: std::io::BufWriter::new(out), core: 0, max, from_pc, armed: from_pc.is_none(), count: 0 } }
}
impl<S: Soc> Observer<S> for RegTrace {
    fn name(&self) -> &'static str { "regtrace" }
    fn wants(&self) -> Wants { Wants::INSN | Wants::NO_IDLE_SKIP }
    fn on_insn(&mut self, _cx: &Ctx, core: usize, cpu: &S::Core, _bus: &mut S::Bus, pc: u32) -> Option<Stop> {
        if core != self.core || cpu.waiting() { return None; }
        if !self.armed && self.from_pc == Some(pc) { self.armed = true; }
        if self.armed && self.count >= self.max { return Some(Stop::Halted); }
        if self.armed { self.count += 1; let _ = writeln!(self.out, "{}", cpu.regtrace_line(pc)); }
        None
    }
}
