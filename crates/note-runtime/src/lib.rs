//! Per-instance runtime (proposal §6): emulation-owner loop and pacing,
//! protocol server, raw/merged logs, capture, AVD storage and locks, host-side
//! networking, GDB stub and the RFC2217 serial bridge.
//!
//! G2a lands the host-side network report and the user-mode forward spec.
//! The owner loop, storage and the protocol server are G3.

pub mod capture;
pub mod forward;
pub mod legacy;
pub mod logs;
pub mod network;
pub mod rfc2217;
pub mod session;
pub mod symbolize;
pub mod store;

pub use capture::{encode_png, FrameCapture};

pub use forward::{parse_forward, ForwardSpec};
pub use legacy::{import, plan, ImportPlan};
pub use network::{ap_subnet_overlaps, incompatible_shared, shared_failure_permission, HostMode, NetworkInfo};
pub use session::{bind_control, serve_connection, transact, Instance, OwnedServer, Queued};
pub use store::Store;
