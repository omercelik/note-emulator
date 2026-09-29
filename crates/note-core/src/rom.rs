//! Mask ROM store: the ROM is checked against known hashes and only ever loaded from the data
//! directory. The esp-rom-elfs ELF is Apache-2.0 and ships with the project (ADR-018), so a
//! missing ROM is imported from the bundled copy automatically; `ndb rom import FILE` still
//! accepts another verified copy.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::paths;

pub const ESP32S3_ROM_FILE: &str = "esp32s3_rev0_rom.elf";

/// SHA-256 of `esp32s3_rev0_rom.elf` in the esp-rom-elfs releases we have verified.
pub const KNOWN_ESP32S3_ROMS: &[(&str, &str)] =
    &[("esp-rom-elfs 20241011", "c0ce0f338d1de1bdc6efbef1591779a2a42c1ab7d759d3c6ae8ae63a7dd34cfd")];

#[derive(Debug, Clone)]
pub struct RomInfo {
    pub path: PathBuf,
    pub sha256: String,
    pub release: &'static str,
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(|e| Error::io(format!("open {}", path.display()), e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf).map_err(|e| Error::io(format!("read {}", path.display()), e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn identify(path: &Path) -> Result<RomInfo> {
    let sha256 = sha256_file(path)?;
    match KNOWN_ESP32S3_ROMS.iter().find(|(_, h)| *h == sha256) {
        Some((release, _)) => Ok(RomInfo { path: path.to_path_buf(), sha256, release }),
        None => Err(Error::RomMismatch { path: path.to_path_buf(), actual: sha256 }),
    }
}

/// Verify `source` and copy it into the ROM store. Returns the stored ROM.
pub fn import(source: &Path) -> Result<RomInfo> {
    identify(source)?;
    let dir = paths::rom_dir();
    fs::create_dir_all(&dir).map_err(|e| Error::io(format!("create {}", dir.display()), e))?;
    let dest = dir.join(ESP32S3_ROM_FILE);
    let temp = dir.join(format!(".{ESP32S3_ROM_FILE}.tmp"));
    fs::copy(source, &temp).map_err(|e| Error::io(format!("copy {}", source.display()), e))?;
    fs::rename(&temp, &dest).map_err(|e| Error::io(format!("install {}", dest.display()), e))?;
    identify(&dest)
}

/// The ROM that ships with the project, found next to the running binary: an app bundle's
/// `Contents/Resources/rom/`, or the repository's `third_party/esp-rom-elfs/` for binaries built
/// in `target/`. No build-time path is compiled in (the bundle audit forbids developer paths).
pub fn bundled() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().skip(1).take(6).find_map(|dir| {
        [dir.join("Resources/rom").join(ESP32S3_ROM_FILE), dir.join("third_party/esp-rom-elfs").join(ESP32S3_ROM_FILE)]
            .into_iter()
            .find(|candidate| candidate.is_file())
    })
}

/// The imported ROM, re-verified on every call. When none is imported yet, the bundled copy is
/// imported first (still hash-checked).
pub fn installed() -> Result<RomInfo> {
    let path = paths::rom_dir().join(ESP32S3_ROM_FILE);
    if !path.is_file() {
        if let Some(source) = bundled() {
            return import(&source);
        }
        return Err(Error::RomMissing { expected: path });
    }
    identify(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_rom_is_found_from_a_target_binary_and_is_the_known_release() {
        // Test binaries live in target/<profile>/deps; the repository copy is a few levels up.
        let rom = bundled().expect("third_party/esp-rom-elfs/esp32s3_rev0_rom.elf");
        assert_eq!(identify(&rom).unwrap().release, "esp-rom-elfs 20241011");
    }
}

