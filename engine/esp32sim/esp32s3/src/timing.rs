//! Receipt-backed timing for the ESP32-S3 shared-time machine.
//!
//! This first slice prices only straight-line instructions fetched from internal SRAM. Aligned
//! 32-bit internal-SRAM loads are accepted when their result is not consumed by the next
//! instruction. Every other event is refused by name.

use crate::bus::{DRAM_HIGH, DRAM_LOW, IRAM_HIGH, IRAM_LOW};
use emu_core::{
    CostModel, ExecutionFacts, LifecycleFacts, LifecycleKind, MemoryAccess, MemoryAccessKind,
    StepKind,
};
use std::cell::RefCell;
use std::rc::Rc;
use xtensa_lx7::Op;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CostTier {
    Exact,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiptId {
    /// IDF 6.1 issue measurement summarized in `tests/fixtures/sram-cost-receipt.json`.
    Idf61StraightLineIssue,
    /// IDF 6.1 SRAM load measurement summarized in `tests/fixtures/sram-cost-receipt.json`.
    Idf61IndependentSramAccess,
    /// IDF 6.1 register-block measurement summarized in
    /// `tests/fixtures/mmio-read-receipt.json`.
    Idf61RegisterBlockRead,
    /// IDF 6.1 opcode-ladder measurement summarized in
    /// `tests/fixtures/opcode-cost-receipt.json`.
    Idf61OpcodeLadders,
}

impl ReceiptId {
    pub const fn file(self) -> &'static str {
        match self {
            Self::Idf61StraightLineIssue | Self::Idf61IndependentSramAccess => {
                "esp32s3/tests/fixtures/sram-cost-receipt.json"
            }
            Self::Idf61RegisterBlockRead => "esp32s3/tests/fixtures/mmio-read-receipt.json",
            Self::Idf61OpcodeLadders => "esp32s3/tests/fixtures/opcode-cost-receipt.json",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MmioReadTier {
    Fast,
    Apb,
    Nrx,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstructionCost {
    Branch { taken: bool },
    Jump,
    JumpRegister,
    LoopSetup,
    Quotient,
    Remainder,
    LoadUse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CostClass {
    InstructionIssue,
    IndependentSramAccess,
    MmioRead(MmioReadTier),
    Instruction(InstructionCost),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CostComponent {
    pub class: CostClass,
    pub tier: CostTier,
    pub cycles: u32,
    pub receipt: ReceiptId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerEntry {
    pub core: usize,
    pub pc: u32,
    pub cycles: u32,
    pub components: Vec<CostComponent>,
}

#[derive(Clone, Debug, Default)]
struct State {
    ledger: Vec<LedgerEntry>,
    previous_load: [Option<u8>; 2],
}

#[derive(Clone, Debug, Default)]
pub struct Esp32S3SramCostModel {
    state: Rc<RefCell<State>>,
}

impl Esp32S3SramCostModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ledger(&self) -> Vec<LedgerEntry> {
        self.state.borrow().ledger.clone()
    }
}

impl CostModel for Esp32S3SramCostModel {
    fn lifecycle(&mut self, facts: &LifecycleFacts) -> Result<(), String> {
        if facts.chip != "esp32s3" || facts.cores != 2 || facts.cpu_hz != 240_000_000 {
            return Err(format!(
                "UnsupportedChipConfig: {} with {} cores at {} Hz (unexplained tier)",
                facts.chip, facts.cores, facts.cpu_hz
            ));
        }
        let mut state = self.state.borrow_mut();
        match facts.kind {
            LifecycleKind::Attach | LifecycleKind::ChipReset => *state = State::default(),
            LifecycleKind::CoreReset(core) if core < state.previous_load.len() => {
                state.previous_load[core] = None;
            }
            LifecycleKind::CoreReset(core) => {
                return Err(format!(
                    "UnsupportedCore({core}): ESP32-S3 has two cores (unexplained tier)"
                ));
            }
        }
        Ok(())
    }

    fn cycles(&mut self, facts: &ExecutionFacts<'_>) -> Result<u32, String> {
        if facts.core >= 2 {
            return Err(format!(
                "UnsupportedCore({}): ESP32-S3 has two cores (unexplained tier)",
                facts.core
            ));
        }
        match facts.outcome.kind {
            StepKind::Retired => {}
            StepKind::Idle => return Err("Idle: cost not adopted (unexplained tier)".into()),
            StepKind::TrapBefore(trap) | StepKind::TrapDuring(trap) => {
                return Err(format!(
                    "Trap::{trap:?}: cost not adopted (unexplained tier)"
                ));
            }
        }
        if let Some(control) = facts.outcome.control {
            return Err(format!(
                "Control::{:?}: cost not adopted (unexplained tier)",
                control.kind
            ));
        }
        let bytes = facts
            .outcome
            .bytes
            .ok_or_else(|| "InstructionBytes: unavailable (unexplained tier)".to_string())?;
        if !(IRAM_LOW..IRAM_HIGH).contains(&facts.outcome.pc) {
            return Err(format!(
                "InstructionFetch({:#010x}): non-SRAM cost not adopted (unexplained tier)",
                facts.outcome.pc
            ));
        }
        let data_accesses = validate_fetch(facts, bytes)?;

        let instruction = xtensa_lx7::decode(facts.outcome.pc, bytes);
        if instruction.len != facts.outcome.length {
            return Err(format!(
                "InstructionLength({:?}): decoded {} but observed {} (unexplained tier)",
                instruction.op, instruction.len, facts.outcome.length
            ));
        }
        let (load_destination, read_registers, expects_load, instruction_timing) =
            classify(instruction.op, instruction.s, instruction.t)?;
        let sequential_pc = facts.outcome.pc.wrapping_add(u32::from(instruction.len));
        let control_flow = facts.outcome.next_pc != sequential_pc;
        if control_flow
            && !matches!(
                instruction_timing,
                InstructionTiming::Branch
                    | InstructionTiming::Jump
                    | InstructionTiming::JumpRegister
            )
        {
            return Err(format!(
                "ControlFlow({:?}): cost not adopted (unexplained tier)",
                instruction.op
            ));
        }
        let instruction_component = price_instruction(instruction_timing, control_flow);
        let mut components = Vec::new();
        let mut next_previous_load = None;
        if expects_load {
            let access = exactly_one_load(data_accesses, instruction.op)?;
            match classify_load(access, instruction.op)? {
                LoadClass::Sram => {
                    next_previous_load = load_destination;
                    components.push(instruction_component);
                    components.push(CostComponent {
                        class: CostClass::IndependentSramAccess,
                        tier: CostTier::Exact,
                        cycles: 0,
                        receipt: ReceiptId::Idf61IndependentSramAccess,
                    });
                }
                LoadClass::Mmio(tier) => components.push(CostComponent {
                    class: CostClass::MmioRead(tier),
                    tier: CostTier::Exact,
                    cycles: match tier {
                        MmioReadTier::Fast => 9,
                        MmioReadTier::Apb => 15,
                        MmioReadTier::Nrx => 18,
                    },
                    receipt: ReceiptId::Idf61RegisterBlockRead,
                }),
            }
        } else if !data_accesses.is_empty() {
            return Err(format!(
                "UnexpectedMemoryAccess({:?}): cost not adopted (unexplained tier)",
                instruction.op
            ));
        } else {
            components.push(instruction_component);
        }

        let mut state = self.state.borrow_mut();
        if let Some(previous) = state.previous_load[facts.core] {
            if read_registers & (1 << previous) != 0 {
                components.push(CostComponent {
                    class: CostClass::Instruction(InstructionCost::LoadUse),
                    tier: CostTier::Exact,
                    cycles: 1,
                    receipt: ReceiptId::Idf61OpcodeLadders,
                });
            }
        }
        let cycles = components.iter().map(|component| component.cycles).sum();
        state.ledger.push(LedgerEntry {
            core: facts.core,
            pc: facts.outcome.pc,
            cycles,
            components,
        });
        state.previous_load[facts.core] = next_previous_load;
        Ok(cycles)
    }
}

fn validate_fetch<'a>(
    facts: &'a ExecutionFacts<'_>,
    bytes: [u8; 4],
) -> Result<&'a [MemoryAccess], String> {
    let Some(fetch) = facts.accesses.first() else {
        return Err("InstructionFetch: missing conceptual fetch (unexplained tier)".into());
    };
    if fetch.kind != MemoryAccessKind::Fetch
        || fetch.address != facts.outcome.pc
        || fetch.width != 4
        || fetch.value != u32::from_le_bytes(bytes)
        || fetch.fault.is_some()
    {
        return Err(
            "InstructionFetch: malformed or faulting conceptual fetch (unexplained tier)".into(),
        );
    }
    let data_accesses = &facts.accesses[1..];
    if data_accesses
        .iter()
        .any(|access| access.kind == MemoryAccessKind::Fetch)
    {
        return Err("InstructionFetch: multiple conceptual fetches (unexplained tier)".into());
    }
    Ok(data_accesses)
}

#[derive(Clone, Copy)]
enum InstructionTiming {
    Issue,
    Branch,
    Jump,
    JumpRegister,
    LoopSetup,
    Quotient,
    Remainder,
}

fn classify(op: Op, s: u8, t: u8) -> Result<(Option<u8>, u16, bool, InstructionTiming), String> {
    use Op::*;
    let bit = |register| 1u16 << register;
    match op {
        L32i | L32iN => Ok((Some(t), bit(s), true, InstructionTiming::Issue)),
        MoviN | Memw => Ok((None, 0, false, InstructionTiming::Issue)),
        Sub | Saltu => Ok((None, bit(s) | bit(t), false, InstructionTiming::Issue)),
        Beqz | Bnez | Bltz | Bgez | BeqzN | BnezN | Beqi | Bnei | Blti | Bgei | Bltui | Bgeui
        | Bbci | Bbsi => Ok((None, bit(s), false, InstructionTiming::Branch)),
        Bnone | Bany | Ball | Bnall | Beq | Bne | Blt | Bge | Bltu | Bgeu | Bbc | Bbs => {
            Ok((None, bit(s) | bit(t), false, InstructionTiming::Branch))
        }
        J => Ok((None, 0, false, InstructionTiming::Jump)),
        Jx => Ok((None, bit(s), false, InstructionTiming::JumpRegister)),
        Loop | Loopnez | Loopgtz => Ok((None, bit(s), false, InstructionTiming::LoopSetup)),
        Quos | Quou => Ok((None, bit(s) | bit(t), false, InstructionTiming::Quotient)),
        Rems | Remu => Ok((None, bit(s) | bit(t), false, InstructionTiming::Remainder)),
        Extw | Rsr | Rsync => Ok((None, 0, false, InstructionTiming::Issue)),
        Movsp | Nsa | Nsau | Sext => Ok((None, bit(s), false, InstructionTiming::Issue)),
        Wsr | Xsr => Ok((None, bit(t), false, InstructionTiming::Issue)),
        Max | Maxu | Min | Minu | Mull | Mulsh | Muluh => {
            Ok((None, bit(s) | bit(t), false, InstructionTiming::Issue))
        }
        _ => Err(format!(
            "Instruction::{op:?}: cost not adopted (unexplained tier)"
        )),
    }
}

fn price_instruction(timing: InstructionTiming, branch_taken: bool) -> CostComponent {
    let (class, cycles, receipt) = match timing {
        InstructionTiming::Issue => (
            CostClass::InstructionIssue,
            1,
            ReceiptId::Idf61StraightLineIssue,
        ),
        InstructionTiming::Branch => (
            CostClass::Instruction(InstructionCost::Branch {
                taken: branch_taken,
            }),
            if branch_taken { 3 } else { 1 },
            ReceiptId::Idf61OpcodeLadders,
        ),
        InstructionTiming::Jump => (
            CostClass::Instruction(InstructionCost::Jump),
            3,
            ReceiptId::Idf61OpcodeLadders,
        ),
        InstructionTiming::JumpRegister => (
            CostClass::Instruction(InstructionCost::JumpRegister),
            6,
            ReceiptId::Idf61OpcodeLadders,
        ),
        InstructionTiming::LoopSetup => (
            CostClass::Instruction(InstructionCost::LoopSetup),
            5,
            ReceiptId::Idf61OpcodeLadders,
        ),
        InstructionTiming::Quotient => (
            CostClass::Instruction(InstructionCost::Quotient),
            4,
            ReceiptId::Idf61OpcodeLadders,
        ),
        InstructionTiming::Remainder => (
            CostClass::Instruction(InstructionCost::Remainder),
            5,
            ReceiptId::Idf61OpcodeLadders,
        ),
    };
    CostComponent {
        class,
        tier: CostTier::Exact,
        cycles,
        receipt,
    }
}

fn exactly_one_load(accesses: &[MemoryAccess], op: Op) -> Result<&MemoryAccess, String> {
    match accesses {
        [access] if access.kind == MemoryAccessKind::Read => Ok(access),
        _ => Err(format!(
            "MemoryShape({op:?}): expected one load (unexplained tier)"
        )),
    }
}

enum LoadClass {
    Sram,
    Mmio(MmioReadTier),
}

fn classify_load(access: &MemoryAccess, op: Op) -> Result<LoadClass, String> {
    if access.fault.is_some() {
        return Err(format!(
            "MemoryFault({op:?}): cost not adopted (unexplained tier)"
        ));
    }
    if access.width != 4 || access.address & 3 != 0 {
        return Err(format!(
            "UnalignedOrNonWordSramLoad({op:?}): cost not adopted (unexplained tier)"
        ));
    }
    if (DRAM_LOW..DRAM_HIGH).contains(&access.address) {
        return Ok(LoadClass::Sram);
    }
    if let Some(tier) = receipted_mmio_read_tier(access.address) {
        return Ok(LoadClass::Mmio(tier));
    }
    match access.address {
        0x6000_703c | 0x6000_8038 => Err(format!(
            "MmioRead({:#010x}): receipt is a distribution with no adopted scalar cost",
            access.address
        )),
        0x6000_0000..=0x600f_ffff => Err(format!(
            "MmioRead({:#010x}): register not covered by the adopted receipt",
            access.address
        )),
        _ => Err(format!(
            "DataAccess({:#010x}): non-SRAM cost not adopted (unexplained tier)",
            access.address
        )),
    }
}

fn receipted_mmio_read_tier(address: u32) -> Option<MmioReadTier> {
    match address {
        0x600c_0060 | 0x600c_1014 | 0x600c_4004 | 0x600c_e05c => Some(MmioReadTier::Fast),
        0x6001_ccd4 => Some(MmioReadTier::Nrx),
        0x6000_001c | 0x6000_2018 | 0x6000_3018 | 0x6000_40b0 | 0x6000_50f0 | 0x6000_90b4
        | 0x6000_e044 | 0x6001_3050 | 0x6001_f070 | 0x6002_0070 | 0x6002_3044 | 0x6002_4014
        | 0x6002_6014 | 0x6003_8008 | 0x6003_b018 | 0x6003_f0a8 | 0x6004_0000 => {
            Some(MmioReadTier::Apb)
        }
        _ => None,
    }
}
