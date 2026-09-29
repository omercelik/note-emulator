//! `--profile`: a per-instruction pc histogram. Exact but slow, and it keeps idle cores stepping
//! (a sleeping core shows as a hot `waiti`); `BlockProfile` is the full-speed alternative.
use crate::observe::{Ctx, Observer, Wants};
use crate::soc::{Soc, Stop};
use std::collections::HashMap;

#[derive(Default)]
pub struct PcHist { pub hist: HashMap<u32, u64>, pub top: usize }
impl PcHist { pub fn new(top: usize) -> Self { PcHist { hist: HashMap::new(), top } } }
impl<S: Soc> Observer<S> for PcHist {
    fn name(&self) -> &'static str { "profile" }
    fn wants(&self) -> Wants { Wants::INSN | Wants::NO_IDLE_SKIP }
    fn on_insn(&mut self, _cx: &Ctx, _core: usize, _cpu: &S::Core, _bus: &mut S::Bus, pc: u32) -> Option<Stop> { *self.hist.entry(pc).or_insert(0) += 1; None }
    fn report(&mut self, cx: &Ctx) -> String {
        let mut v: Vec<(u32, u64)> = self.hist.iter().map(|(a, c)| (*a, *c)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let total: u64 = v.iter().map(|x| x.1).sum();
        let mut s = format!("[profile] top {} pcs of {} instructions\n", self.top, total);
        for (a, c) in v.iter().take(self.top) { s += &format!("  {:08x} {:>6.2}%  {}\n", a, *c as f64 * 100.0 / total as f64, cx.sym(*a)); }
        s
    }
}
