//! Derived peripheral clocks. The CPU clock is the time base; each slower domain is an integer
//! divider of it, delivered with "done" accounting so a domain never loses or gains a tick to
//! rounding when the CPU advances in odd-sized batches. The divider table is passed at each call
//! as a `const` array so the divisions fold to multiplies in the caller.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClockDomain { Cpu, Apb, Systimer, RtcSlow, Xtal }

pub type Dividers<const N: usize> = [(ClockDomain, u64); N];

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ClockTree<const N: usize> { pub cpu_hz: u64, cycle_total: u64, #[serde(with = "crate::snap::arr")] done: [u64; N] }

impl<const N: usize> ClockTree<N> {
    pub const fn new(cpu_hz: u64) -> Self { ClockTree { cpu_hz, cycle_total: 0, done: [0; N] } }
    pub fn cycles(&self) -> u64 { self.cycle_total }
    /// Advance by `cycles` CPU cycles and report every domain that gained ticks, in `divs` order.
    #[inline(always)]
    pub fn advance(&mut self, divs: &Dividers<N>, cycles: u64, mut f: impl FnMut(ClockDomain, u64)) {
        self.cycle_total += cycles;
        let mut i = 0;
        while i < N {
            let now = self.cycle_total / divs[i].1;
            if now > self.done[i] { f(divs[i].0, now - self.done[i]); self.done[i] = now; }
            i += 1;
        }
    }
    /// CPU cycles per tick of `d` (1 if `d` is not in the table).
    pub const fn divider(divs: &Dividers<N>, d: ClockDomain) -> u64 { divider(divs, d) }
}

/// CPU cycles per tick of `d` in a divider table (1 if `d` is not in it).
pub const fn divider(divs: &[(ClockDomain, u64)], d: ClockDomain) -> u64 {
    let mut i = 0;
    while i < divs.len() { if divs[i].0 as u8 == d as u8 { return divs[i].1; } i += 1; }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delivered_ticks_never_drift() {
        const D: Dividers<2> = [(ClockDomain::Systimer, 15), (ClockDomain::Apb, 3)];
        let mut t = ClockTree::<2>::new(240_000_000);
        let (mut st, mut apb) = (0, 0);
        for c in [1u64, 7, 64, 512, 5, 3, 100] { t.advance(&D, c, |d, n| match d { ClockDomain::Systimer => st += n, ClockDomain::Apb => apb += n, _ => {} }); }
        assert_eq!(st, 692 / 15); assert_eq!(apb, 692 / 3); assert_eq!(t.cycles(), 692);
        assert_eq!(ClockTree::<2>::divider(&D, ClockDomain::Apb), 3); assert_eq!(ClockTree::<2>::divider(&D, ClockDomain::Cpu), 1);
    }
}
