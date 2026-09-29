//! `--regstat FILE`: per-(register, pc, direction) access counts with the last value, for
//! reverse-engineering what a firmware expects of a peripheral. Rows go to `path`, sorted by count.
use crate::observe::{Ctx, Observer, Wants};
use crate::soc::Soc;
use std::collections::HashMap;
use std::io::Write;

pub struct MmioHeat { pub path: String, pub stat: HashMap<(u32, u32, bool), (u64, u32)>, pub names: fn(u32) -> String }
impl MmioHeat {
    /// `names` renders an address as `BLOCK+0xoff`.
    pub fn new(path: &str, names: fn(u32) -> String) -> Self { MmioHeat { path: path.to_string(), stat: HashMap::new(), names } }
}
impl<S: Soc> Observer<S> for MmioHeat {
    fn name(&self) -> &'static str { "regstat" }
    fn wants(&self) -> Wants { Wants::MMIO }
    fn on_mmio(&mut self, _cx: &Ctx, pc: u32, addr: u32, value: u32, write: bool) {
        let e = self.stat.entry((addr, pc, write)).or_insert((0, 0)); e.0 += 1; e.1 = value;
    }
    fn report(&mut self, cx: &Ctx) -> String {
        let mut rows: Vec<_> = self.stat.iter().collect(); rows.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
        let Ok(f) = std::fs::File::create(&self.path) else { return format!("[emu] regstat: cannot write {}", self.path) };
        let mut f = std::io::BufWriter::new(f);
        let _ = writeln!(f, "# count kind block+off addr last_value pc symbol");
        for (&(addr, pc, wr), &(n, val)) in rows { let _ = writeln!(f, "{} {} {} {:#010x} {:#010x} {:#010x} {}", n, if wr { "wr" } else { "rd" }, (self.names)(addr), addr, val, pc, cx.sym(pc)); }
        format!("[emu] wrote {} register access rows to {}", self.stat.len(), self.path)
    }
}
