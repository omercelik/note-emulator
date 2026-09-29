//! RV32IMC / RV32IMAC: the core in the ESP32-C3 and, with the A extension, the ESP32-C6.
//!
//! Machine mode only, no MMU, no F/D extensions — which is exactly what the silicon has.
//! Interrupts are *not* the standard `mie`/`mip` external-interrupt model: the C3 routes 62
//! peripheral sources through its own interrupt matrix onto 31 CPU lines and vectors each one
//! separately (`mtvec` in vectored mode), so the SoC hands us the pending line and we take it.
pub mod bus { pub use emu_core::bus::*; }
pub mod core;
pub mod decode;
pub mod disasm;
pub mod exec;
pub mod state;

pub use emu_core::{Bus, Core, Fault, FlatRam};
pub use decode::{decode, Insn, Op};
pub use exec::{step, Trap};
pub use state::Cpu;
