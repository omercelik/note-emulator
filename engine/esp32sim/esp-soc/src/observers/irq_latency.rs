//! `--irq-latency`: cycles from a line being raised at a core to the core taking it, per line.
//! A line still asserted when the interrupt is taken is a level source served late; edge/timer
//! lines the core raises itself are counted from when they appear on its input.
use crate::observe::{Ctx, Observer, Wants};
use crate::soc::Soc;
use emu_core::Trap;

#[derive(Default, Clone, Copy)]
struct Stat { n: u64, total: u64, max: u64 }
pub struct IrqLatency { raised: Vec<[Option<u64>; 32]>, stat: Vec<[Stat; 32]> }
impl IrqLatency { pub fn new(cores: usize) -> Self { IrqLatency { raised: vec![[None; 32]; cores], stat: vec![[Stat::default(); 32]; cores] } } }
impl<S: Soc> Observer<S> for IrqLatency {
    fn name(&self) -> &'static str { "irq-latency" }
    fn wants(&self) -> Wants { Wants::IRQ | Wants::TRAP }
    fn on_irq_raised(&mut self, cx: &Ctx, core: usize, line: u32) {
        if let Some(r) = self.raised.get_mut(core) { if r[(line & 31) as usize].is_none() { r[(line & 31) as usize] = Some(cx.cycles); } }
    }
    fn on_trap(&mut self, cx: &Ctx, core: usize, _cpu: &S::Core, _pc: u32, trap: &Trap) {
        let Trap::Interrupt(line) = trap else { return };
        let (Some(r), Some(st)) = (self.raised.get_mut(core), self.stat.get_mut(core)) else { return };
        if let Some(t0) = r[(line & 31) as usize].take() {
            let d = cx.cycles.saturating_sub(t0);
            let s = &mut st[(line & 31) as usize]; s.n += 1; s.total += d; s.max = s.max.max(d);
        }
    }
    fn report(&mut self, cx: &Ctx) -> String {
        let mut s = String::from("[irq-latency] per core, line: taken, mean cycles, max cycles (raised -> taken)\n");
        for (core, st) in self.stat.iter().enumerate() {
            for (line, x) in st.iter().enumerate() {
                if x.n == 0 { continue; }
                s += &format!("  core{} int{:<2} {:>9}  mean {:>8.1}  max {:>8}  ({:.2} us max)\n", core, line, x.n, x.total as f64 / x.n as f64, x.max, x.max as f64 * 1e6 / cx.cpu_hz as f64);
            }
        }
        s
    }
}
