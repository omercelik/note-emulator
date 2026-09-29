//! `--trace`: one line per instruction with the core's registers, plus taken traps.
use crate::observe::{Ctx, Observer, Wants};
use crate::soc::{Soc, Stop};
use emu_core::{Bus, Core, Trap};

pub struct Trace { pub from: u64 }
impl<S: Soc> Observer<S> for Trace {
    fn name(&self) -> &'static str { "trace" }
    fn wants(&self) -> Wants { Wants::INSN | Wants::TRAP | Wants::NO_IDLE_SKIP }
    fn on_insn(&mut self, cx: &Ctx, core: usize, cpu: &S::Core, bus: &mut S::Bus, pc: u32) -> Option<Stop> {
        if cpu.insn_count() < self.from { return None; }
        if let Ok(b) = bus.fetch(pc) {
            eprintln!("{}{:>10} {:08x}: {:<w$} {}  {}", if core == 1 { "C1 " } else { "" }, cpu.insn_count(), pc, cpu.disasm(pc, b), cx.sym(pc), cpu.trace_regs(), w = S::Core::TRACE_WIDTH);
        }
        None
    }
    fn on_trap(&mut self, _cx: &Ctx, core: usize, cpu: &S::Core, pc: u32, trap: &Trap) {
        if let Some(l) = cpu.trace_trap(core, pc, trap) { eprintln!("{}", l); }
    }
}
