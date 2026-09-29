//! Types shared by every NOTE Emulator component: hardware profiles, data
//! locations, the mask ROM store, and stable error codes.

pub mod error;
pub mod flash;
pub mod paths;
pub mod profile;
pub mod rom;

pub use error::{Error, ErrorCode, Result};
pub use profile::{DisplayFormat, PanelKind, PanelVariant, Profile};
