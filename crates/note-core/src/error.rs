use std::path::PathBuf;

/// Stable, machine-readable error codes (Spec §12.2). The string form is part
/// of the protocol and the CLI's `--json` output; never rename a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    ProfileInvalid,
    ProfileUnknown,
    RomMissing,
    RomMismatch,
    FirmwareLayoutUnknown,
    Io,
    DeviceAlreadyRunning,
    AvdNotFound,
    BadRequest,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::ProfileInvalid => "ProfileInvalid",
            ErrorCode::ProfileUnknown => "ProfileUnknown",
            ErrorCode::RomMissing => "RomMissing",
            ErrorCode::RomMismatch => "RomMismatch",
            ErrorCode::FirmwareLayoutUnknown => "FirmwareLayoutUnknown",
            ErrorCode::Io => "Io",
            ErrorCode::DeviceAlreadyRunning => "DeviceAlreadyRunning",
            ErrorCode::AvdNotFound => "AvdNotFound",
            ErrorCode::BadRequest => "BadRequest",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("profile {path}: {reason}")]
    ProfileInvalid { path: PathBuf, reason: String },
    #[error("unknown profile {0:?}")]
    ProfileUnknown(String),
    #[error("ESP32-S3 mask ROM not imported; expected {expected}. Run `ndb rom import <path>/esp32s3_rev0_rom.elf` (from ESP-IDF's esp-rom-elfs)")]
    RomMissing { expected: PathBuf },
    #[error("{path} is not a known ESP32-S3 rev0 ROM ELF (sha256 {actual})")]
    RomMismatch { path: PathBuf, actual: String },
    #[error("firmware layout of {path}: {reason}")]
    FirmwareLayout { path: PathBuf, reason: String },
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("AVD {id} is already running (pid {pid})")]
    DeviceAlreadyRunning { id: String, pid: u32 },
    #[error("no AVD {0}")]
    AvdNotFound(String),
    #[error("{0}")]
    BadRequest(String),
}

impl Error {
    pub fn code(&self) -> ErrorCode {
        match self {
            Error::ProfileInvalid { .. } => ErrorCode::ProfileInvalid,
            Error::ProfileUnknown(_) => ErrorCode::ProfileUnknown,
            Error::RomMissing { .. } => ErrorCode::RomMissing,
            Error::RomMismatch { .. } => ErrorCode::RomMismatch,
            Error::FirmwareLayout { .. } => ErrorCode::FirmwareLayoutUnknown,
            Error::Io { .. } => ErrorCode::Io,
            Error::DeviceAlreadyRunning { .. } => ErrorCode::DeviceAlreadyRunning,
            Error::AvdNotFound(_) => ErrorCode::AvdNotFound,
            Error::BadRequest(_) => ErrorCode::BadRequest,
        }
    }

    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io { context: context.into(), source }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
