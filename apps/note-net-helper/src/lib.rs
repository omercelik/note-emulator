//! Privileged setup-address helper (decision D10, Spec §8.3–8.5).
//!
//! The helper adds a `192.168.4.1/32` loopback alias, binds port 80 on that
//! address, and passes the listening fd to `note-emu`. It never reads the
//! traffic. Leases are reconciled from a journal; a missed heartbeat is not
//! proof that the socket was released.

mod book;
mod client;
mod passfd;
mod platform;
mod proto;
mod server;
mod vmnet;

pub use book::{code_str, Auth, Code, HostView};
pub use client::{request_shared, Session, SharedRefusal};
pub use platform::{write_auth, SystemPlatform};
pub use server::{serve, Helper};
pub use vmnet::{probe_shared, SharedProbe};

use std::path::PathBuf;

/// Use the installed per-user system socket when present; otherwise use the
/// development helper's data-home socket (or `/tmp` if too long for `sun_path`).
pub fn default_socket() -> PathBuf {
    let installed = system_socket(unsafe { libc::getuid() });
    if installed.exists() { return installed; }
    let preferred = note_core::paths::data_home().join("helper/helper.sock");
    if preferred.as_os_str().len() < 100 { preferred } else { PathBuf::from("/tmp/note-net-helper.sock") }
}

/// Socket published by an installed root helper for one local user.
pub fn system_socket(uid: u32) -> PathBuf {
    PathBuf::from(format!("/var/run/note-emulator-{uid}.sock"))
}

/// Root-owned authorization and lease journal for one local user.
pub fn system_state_dir(uid: u32) -> PathBuf {
    PathBuf::from(format!("/Library/Application Support/NOTE Emulator/helper/{uid}"))
}

pub fn state_dir() -> PathBuf {
    note_core::paths::data_home().join("helper")
}
