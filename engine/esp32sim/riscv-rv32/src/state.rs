//! Machine-mode CPU state. Only M-mode exists on the C3, so there is no privilege stack beyond
//! `mstatus.MPP` and no address translation.

use std::collections::HashMap;

pub mod csr {
    pub const MSTATUS: u32 = 0x300; pub const MISA: u32 = 0x301; pub const MIE: u32 = 0x304;
    pub const MTVEC: u32 = 0x305; pub const MSCRATCH: u32 = 0x340; pub const MEPC: u32 = 0x341;
    pub const MCAUSE: u32 = 0x342; pub const MTVAL: u32 = 0x343; pub const MIP: u32 = 0x344;
    pub const MCYCLE: u32 = 0xB00; pub const MINSTRET: u32 = 0xB02;
    pub const MCYCLEH: u32 = 0xB80; pub const MINSTRETH: u32 = 0xB82;
    pub const MVENDORID: u32 = 0xF11; pub const MARCHID: u32 = 0xF12; pub const MIMPID: u32 = 0xF13;
    pub const MHARTID: u32 = 0xF14;
    /// Espressif performance counters. The machine-mode set is what `esp_cpu_get_cycle_count`
    /// uses; the user-mode alias at 0x802 is what the ROM's `ets_delay_us` busy-waits on.
    pub const MPCER: u32 = 0x7E0; pub const MPCMR: u32 = 0x7E1; pub const MPCCR: u32 = 0x7E2;
    pub const PCER_U: u32 = 0x800; pub const PCMR_U: u32 = 0x801; pub const PCCR_U: u32 = 0x802;
}

pub mod exc {
    pub const INSN_MISALIGNED: u32 = 0; pub const INSN_ACCESS_FAULT: u32 = 1;
    pub const ILLEGAL_INSN: u32 = 2; pub const BREAKPOINT: u32 = 3;
    pub const LOAD_MISALIGNED: u32 = 4; pub const LOAD_ACCESS_FAULT: u32 = 5;
    pub const STORE_MISALIGNED: u32 = 6; pub const STORE_ACCESS_FAULT: u32 = 7;
    pub const ECALL_M: u32 = 11;
}

pub mod mstatus {
    pub const MIE: u32 = 1 << 3;
    pub const MPIE: u32 = 1 << 7;
    pub const MPP: u32 = 3 << 11;
}

/// Reset vector: the C3 and the C6 start executing in the mask ROM.
pub const RESET_VECTOR: u32 = 0x4000_0000;
/// `misa` for the two cores: RV32IMC (bits I|M|C, MXL=1) and RV32IMAC.
pub const MISA_RV32IMC: u32 = 0x4000_1104;
pub const MISA_RV32IMAC: u32 = 0x4000_1105;

pub struct Cpu {
    pub pc: u32,
    pub x: [u32; 32],
    pub mstatus: u32,
    pub mtvec: u32,
    pub mepc: u32,
    pub mcause: u32,
    pub mtval: u32,
    pub mie: u32,
    pub mscratch: u32,
    /// Host-facing scheduling work. This remains monotonic across reset and includes skipped idle
    /// cycles so existing run statistics keep their meaning.
    pub insn_count: u64,
    /// Architecturally retired instructions. Traps accepted between instructions and trapping
    /// instructions do not increment this counter.
    pub retired_count: u64,
    /// Architectural cycles, including timing-only advancement while no instruction retires.
    pub cycle_count: u64,
    /// Counter bases at the last reset. Guest counters restart at zero while the host-facing
    /// monotonic counters do not.
    pub cycle_base: u64,
    pub instret_base: u64,
    /// halted by WFI until the SoC raises a line
    pub waiting: bool,
    /// the line the SoC's interrupt controller wants taken, if any (`Core::set_irq`)
    pub irq: Option<u32>,
    /// which ISA `misa` reports: IMC on the C3, IMAC on the C6 (execution accepts A on both)
    pub misa: u32,
    /// the address an `lr.w` reserved, until the next `sc.w`
    pub reservation: Option<u32>,
    /// pcs the machine intercepts (stubs, probes), as a bloom over `emu_core::pc_bit`: `run` stops there
    pub boundary_bloom: u64,
    /// CSRs we do not model: read back what was written, like the SoC's unknown registers
    pub csr_other: HashMap<u32, u32>,
}

impl Default for Cpu {
    fn default() -> Self { Self::new() }
}

impl Cpu {
    pub fn new() -> Self {
        Cpu {
            pc: RESET_VECTOR, x: [0; 32],
            mstatus: 0x1800,          // MPP = machine
            mtvec: 0, mepc: 0, mcause: 0, mtval: 0, mie: 0, mscratch: 0,
            insn_count: 0, retired_count: 0, cycle_count: 0, cycle_base: 0, instret_base: 0, waiting: false, irq: None, misa: MISA_RV32IMC, reservation: None, boundary_bloom: 0, csr_other: HashMap::new(),
        }
    }
    pub fn new_rv32imac() -> Self { let mut c = Cpu::new(); c.misa = MISA_RV32IMAC; c }

    pub fn reset(&mut self) {
        // `insn_count` is the emulator's own monotonic counter — it drives the run statistics and
        // the trace, so a chip reset must not rewind it (a reset that did made the browser UI
        // report a negative instruction rate). The architectural cycle counter is separately
        // monotonic and firmware reads its reset-relative value.
        let (insns, retired, cycles, misa, bloom) = (self.insn_count, self.retired_count, self.cycle_count, self.misa, self.boundary_bloom);
        *self = Cpu::new();
        self.insn_count = insns; self.retired_count = retired; self.cycle_count = cycles;
        self.cycle_base = cycles; self.instret_base = retired;
        self.misa = misa; self.boundary_bloom = bloom;
    }

    #[inline(always)]
    pub fn get(&self, r: u8) -> u32 { self.x[r as usize] }
    #[inline(always)]
    pub fn set(&mut self, r: u8, v: u32) { if r != 0 { self.x[r as usize] = v; } }

    #[inline(always)]
    pub fn mie_enabled(&self) -> bool { self.mstatus & mstatus::MIE != 0 }

    pub fn read_csr(&mut self, n: u32) -> u32 {
        use csr::*;
        match n {
            MSTATUS => self.mstatus,
            MISA => self.misa,
            MIE => self.mie,
            MTVEC => self.mtvec,
            MSCRATCH => self.mscratch,
            MEPC => self.mepc,
            MCAUSE => self.mcause,
            MTVAL => self.mtval,
            MIP => 0,                               // the SoC's INTC holds pending state, not `mip`
            MCYCLE | MPCCR | PCCR_U => self.cycle_count.wrapping_sub(self.cycle_base) as u32,
            MCYCLEH => (self.cycle_count.wrapping_sub(self.cycle_base) >> 32) as u32,
            MINSTRET => self.retired_count.wrapping_sub(self.instret_base) as u32,
            MINSTRETH => (self.retired_count.wrapping_sub(self.instret_base) >> 32) as u32,
            MVENDORID => 0, MARCHID => 0, MIMPID => 0, MHARTID => 0,
            _ => self.csr_other.get(&n).copied().unwrap_or(0),
        }
    }

    pub fn write_csr(&mut self, n: u32, v: u32) {
        use csr::*;
        match n {
            MSTATUS => self.mstatus = v,
            MIE => self.mie = v,
            MTVEC => self.mtvec = v,
            MSCRATCH => self.mscratch = v,
            MEPC => self.mepc = v & !1,
            MCAUSE => self.mcause = v,
            MTVAL => self.mtval = v,
            // a write moves the guest-visible counter; the emulator's own count stays monotonic
            MCYCLE | MPCCR | PCCR_U => { let c = self.cycle_count.wrapping_sub(self.cycle_base) & !0xffff_ffff | v as u64; self.cycle_base = self.cycle_count.wrapping_sub(c); }
            MCYCLEH => { let c = self.cycle_count.wrapping_sub(self.cycle_base) & 0xffff_ffff | ((v as u64) << 32); self.cycle_base = self.cycle_count.wrapping_sub(c); }
            MINSTRET => { let c = self.retired_count.wrapping_sub(self.instret_base) & !0xffff_ffff | v as u64; self.instret_base = self.retired_count.wrapping_sub(c); }
            MINSTRETH => { let c = self.retired_count.wrapping_sub(self.instret_base) & 0xffff_ffff | ((v as u64) << 32); self.instret_base = self.retired_count.wrapping_sub(c); }
            MISA | MIP | MVENDORID | MARCHID | MIMPID | MHARTID => {}   // read-only
            _ => { self.csr_other.insert(n, v); }
        }
    }

    /// Enter the trap handler. `cause` has bit 31 set for interrupts; `tval` goes to `mtval`.
    /// `mtvec` mode 1 vectors interrupts to `base + 4*line` — how the C3 dispatches its 31 lines —
    /// while exceptions always enter at the base.
    pub fn trap(&mut self, cause: u32, tval: u32, epc: u32) {
        self.mepc = epc;
        self.mcause = cause;
        self.mtval = tval;
        let mpie = if self.mstatus & mstatus::MIE != 0 { mstatus::MPIE } else { 0 };
        self.mstatus = (self.mstatus & !(mstatus::MIE | mstatus::MPIE)) | mpie | mstatus::MPP;
        let base = self.mtvec & !3;
        self.pc = if self.mtvec & 3 == 1 && cause & 0x8000_0000 != 0 {
            base.wrapping_add(4 * (cause & 0x1f))
        } else {
            base
        };
        self.waiting = false;
    }

    /// `mret`: restore the interrupt-enable bit and jump back.
    pub fn mret(&mut self) {
        let mpie = self.mstatus & mstatus::MPIE != 0;
        self.mstatus = (self.mstatus & !mstatus::MIE) | if mpie { mstatus::MIE } else { 0 } | mstatus::MPIE;
        self.pc = self.mepc;
    }
}
