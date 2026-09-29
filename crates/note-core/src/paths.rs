use std::path::PathBuf;

/// Root of all per-user emulator state (Spec §10):
/// `~/Library/Application Support/NOTE Emulator/`, overridable with `NOTE_EMU_HOME`.
pub fn data_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("NOTE_EMU_HOME") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/NOTE Emulator")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("note-emulator")
    }
}

pub fn rom_dir() -> PathBuf {
    data_home().join("rom")
}

pub fn avd_dir() -> PathBuf {
    data_home().join("avd")
}
