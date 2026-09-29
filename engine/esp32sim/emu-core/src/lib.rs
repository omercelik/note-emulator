//! The contract between a CPU core and the SoC around it, shared by every core crate:
//! `Bus` (memory + the hooks generated code needs), `Core` (what a machine drives), `Trap`
//! (why a step stopped), `ClockTree` (derived peripheral clocks), and the AArch64 encoder any
//! JIT can emit through. No dependencies, no chip knowledge.
pub mod bus;
pub mod clock;
pub mod core;
pub mod snap;
#[cfg(all(target_arch = "aarch64", any(target_os = "macos", target_os = "linux")))]
pub mod jit_a64;

pub use bus::{Bus, Fault, FlatRam};
pub use clock::{ClockDomain, ClockTree, Dividers};
pub use core::{
    CacheOperation, ControlEvent, ControlEventKind, Core, CostModel, ExecutionFacts,
    LifecycleFacts, LifecycleKind, MemoryAccess, MemoryAccessKind, StepKind, StepOutcome,
    TlbOperation, Trap,
};
