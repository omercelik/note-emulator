//! Engine adapter and NOTE board models (proposal §4–5). NOTE-specific hardware lives here,
//! outside the vendored engine; the engine only gets generic SoC changes (ADR-006, Spec §4.3).

pub mod board;
mod gray16;
pub mod gdb;
pub mod machine;
pub mod pcf8563;
pub mod replay;
pub mod snapshot;
pub mod ssd2683;
pub mod es8311;
pub mod legacy;

pub use machine::{Console, NoteMachine, RadioConfig, SliceEnd, SocBusExt, CPU_HZ};
pub use replay::{Guest, ReplayMachine};
