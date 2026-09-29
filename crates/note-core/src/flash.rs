//! Firmware layout import (Spec §5.3, FW-01): a merged flash image, or an ESP-IDF build
//! directory with `flasher_args.json`. Never guesses that a lone `.bin` belongs at offset 0.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

const IMAGE_MAGIC: u8 = 0xE9;
const PARTITION_TABLE_OFFSET: usize = 0x8000;
const PARTITION_MAGIC: [u8; 2] = [0xAA, 0x50];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    MergedImage,
    BuildDirectory,
}

/// A validated flash image of exactly the profile's flash size.
#[derive(Debug)]
pub struct FlashImage {
    pub layout: Layout,
    pub bytes: Vec<u8>,
    /// (offset, file, length) of every placed segment.
    pub segments: Vec<(usize, PathBuf, usize)>,
}

#[derive(Deserialize)]
struct FlasherArgs {
    flash_files: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    extra_esptool_args: Option<ExtraArgs>,
}

#[derive(Deserialize)]
struct ExtraArgs {
    chip: Option<String>,
}

fn layout_error(path: &Path, reason: impl Into<String>) -> Error {
    Error::FirmwareLayout { path: path.to_path_buf(), reason: reason.into() }
}

pub fn load(source: &Path, flash_size: usize) -> Result<FlashImage> {
    let meta = fs::metadata(source).map_err(|e| Error::io(format!("open {}", source.display()), e))?;
    if meta.is_dir() {
        load_build_dir(source, flash_size)
    } else {
        load_merged(source, flash_size)
    }
}

fn load_merged(path: &Path, flash_size: usize) -> Result<FlashImage> {
    let data = fs::read(path).map_err(|e| Error::io(format!("read {}", path.display()), e))?;
    if data.len() > flash_size {
        return Err(layout_error(path, format!("{} bytes exceed the {flash_size}-byte flash", data.len())));
    }
    if data.first() != Some(&IMAGE_MAGIC) {
        return Err(layout_error(path, "no ESP image header at offset 0"));
    }
    if data.get(PARTITION_TABLE_OFFSET..PARTITION_TABLE_OFFSET + 2) != Some(&PARTITION_MAGIC[..]) {
        return Err(layout_error(
            path,
            "no partition table at 0x8000: this looks like a standalone application image; supply a merged image or the ESP-IDF build directory",
        ));
    }
    let len = data.len();
    let mut bytes = vec![0xFF; flash_size];
    bytes[..len].copy_from_slice(&data);
    Ok(FlashImage { layout: Layout::MergedImage, bytes, segments: vec![(0, path.to_path_buf(), len)] })
}

fn load_build_dir(dir: &Path, flash_size: usize) -> Result<FlashImage> {
    let manifest = dir.join("flasher_args.json");
    let text = fs::read_to_string(&manifest).map_err(|e| Error::io(format!("read {}", manifest.display()), e))?;
    let args: FlasherArgs = serde_json::from_str(&text).map_err(|e| layout_error(&manifest, e.to_string()))?;
    match args.extra_esptool_args.and_then(|a| a.chip) {
        Some(chip) if chip == "esp32s3" => {}
        Some(chip) => return Err(layout_error(&manifest, format!("built for {chip}, not esp32s3"))),
        None => return Err(layout_error(&manifest, "no target chip in flasher_args.json")),
    }
    let mut spans = Vec::new();
    for (offset, file) in &args.flash_files {
        let offset = parse_offset(offset).ok_or_else(|| layout_error(&manifest, format!("bad offset {offset:?}")))?;
        let path = dir.join(file);
        let data = fs::read(&path).map_err(|e| Error::io(format!("read {}", path.display()), e))?;
        spans.push((offset, path, data));
    }
    spans.sort_by_key(|(offset, ..)| *offset);
    let mut bytes = vec![0xFF; flash_size];
    let mut segments = Vec::new();
    let mut end_of_previous = 0;
    for (offset, path, data) in spans {
        let end = offset.checked_add(data.len()).filter(|&e| e <= flash_size)
            .ok_or_else(|| layout_error(&path, format!("segment at {offset:#x} runs past the flash")))?;
        if offset < end_of_previous {
            return Err(layout_error(&path, format!("segment at {offset:#x} overlaps the previous one")));
        }
        bytes[offset..end].copy_from_slice(&data);
        segments.push((offset, path, data.len()));
        end_of_previous = end;
    }
    if !segments.iter().any(|(o, ..)| *o == 0) {
        return Err(layout_error(&manifest, "no bootloader at offset 0"));
    }
    Ok(FlashImage { layout: Layout::BuildDirectory, bytes, segments })
}

fn parse_offset(text: &str) -> Option<usize> {
    let t = text.trim();
    match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => usize::from_str_radix(hex, 16).ok(),
        None => t.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: usize = 1 << 20;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("note-core-flash-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn merged(len: usize) -> Vec<u8> {
        let mut data = vec![0xFF; len];
        data[0] = IMAGE_MAGIC;
        data[PARTITION_TABLE_OFFSET..PARTITION_TABLE_OFFSET + 2].copy_from_slice(&PARTITION_MAGIC);
        data
    }

    #[test]
    fn merged_image_is_padded_to_the_flash_size() {
        let dir = temp_dir("merged");
        let path = dir.join("fw.bin");
        fs::write(&path, merged(0x10000)).unwrap();
        let img = load(&path, 16 * MIB).unwrap();
        assert_eq!(img.layout, Layout::MergedImage);
        assert_eq!(img.bytes.len(), 16 * MIB);
        assert_eq!(img.bytes[0], IMAGE_MAGIC);
        assert_eq!(img.bytes[0x10000], 0xFF);
    }

    #[test]
    fn app_only_and_oversized_images_are_refused() {
        let dir = temp_dir("refuse");
        let app = dir.join("app.bin");
        let mut data = vec![0u8; 0x9000];
        data[0] = IMAGE_MAGIC;
        fs::write(&app, &data).unwrap();
        let err = load(&app, 16 * MIB).unwrap_err().to_string();
        assert!(err.contains("standalone application"), "{err}");
        let big = dir.join("big.bin");
        fs::write(&big, merged(2 * MIB + 1)).unwrap();
        assert!(load(&big, 2 * MIB).unwrap_err().to_string().contains("exceed"));
    }

    fn build_dir(name: &str, chip: &str, files: &[(&str, &str, usize)]) -> PathBuf {
        let dir = temp_dir(name);
        let mut entries = Vec::new();
        for (offset, file, len) in files {
            let path = dir.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, vec![0xA5; *len]).unwrap();
            entries.push(format!("\"{offset}\": \"{file}\""));
        }
        fs::write(dir.join("flasher_args.json"),
                  format!("{{\"flash_files\": {{{}}}, \"extra_esptool_args\": {{\"chip\": \"{chip}\"}}}}", entries.join(","))).unwrap();
        dir
    }

    #[test]
    fn build_directory_places_segments_and_rejects_overlap_and_wrong_chip() {
        let ok = build_dir("ok", "esp32s3", &[("0x0", "bootloader/bootloader.bin", 100), ("0x8000", "pt.bin", 16), ("0x10000", "app.bin", 50)]);
        let img = load(&ok, 16 * MIB).unwrap();
        assert_eq!(img.layout, Layout::BuildDirectory);
        assert_eq!((img.bytes[0x8000], img.bytes[0x8010], img.bytes[0x10031], img.bytes[0x10032]), (0xA5, 0xFF, 0xA5, 0xFF));

        let overlap = build_dir("overlap", "esp32s3", &[("0x0", "a.bin", 0x9000), ("0x8000", "b.bin", 16)]);
        assert!(load(&overlap, 16 * MIB).unwrap_err().to_string().contains("overlaps"));

        let c3 = build_dir("c3", "esp32c3", &[("0x0", "a.bin", 16)]);
        assert!(load(&c3, 16 * MIB).unwrap_err().to_string().contains("esp32c3"));

        let missing = build_dir("missing", "esp32s3", &[("0x0", "a.bin", 16)]);
        fs::remove_file(missing.join("a.bin")).unwrap();
        assert!(load(&missing, 16 * MIB).is_err());
    }
}

/// One entry of the ESP-IDF partition table at 0x8000 (32 bytes: magic AA 50, type, subtype,
/// offset, size, label, flags).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub kind: u8,
    pub subtype: u8,
    pub offset: u32,
    pub size: u32,
    pub label: String,
}

pub const PART_TYPE_DATA: u8 = 0x01;
pub const PART_SUBTYPE_COREDUMP: u8 = 0x03;

/// The partition table in a flash image; stops at the first entry that is not an entry
/// (the MD5 record `EB EB` or erased flash).
pub fn partitions(flash: &[u8]) -> Vec<Partition> {
    let mut out = Vec::new();
    let table = flash.get(PARTITION_TABLE_OFFSET..PARTITION_TABLE_OFFSET + 0xC00).unwrap_or(&[]);
    for e in table.chunks_exact(32) {
        if e[0..2] != PARTITION_MAGIC {
            break;
        }
        let le = |i: usize| u32::from_le_bytes(e[i..i + 4].try_into().unwrap());
        let label = e[12..28].iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
        out.push(Partition { kind: e[2], subtype: e[3], offset: le(4), size: le(8), label });
    }
    out
}

/// The raw bytes of the `coredump` data partition, if the table has one and it was written
/// (an erased partition reads 0xFF).
pub fn coredump(flash: &[u8]) -> Option<&[u8]> {
    let p = partitions(flash).into_iter().find(|p| p.kind == PART_TYPE_DATA && p.subtype == PART_SUBTYPE_COREDUMP)?;
    let data = flash.get(p.offset as usize..(p.offset + p.size) as usize)?;
    (data[..4] != [0xFF; 4]).then_some(data)
}

#[cfg(test)]
mod partition_tests {
    use super::*;

    fn entry(kind: u8, subtype: u8, offset: u32, size: u32, label: &str) -> Vec<u8> {
        let mut e = vec![0xAA, 0x50, kind, subtype];
        e.extend_from_slice(&offset.to_le_bytes());
        e.extend_from_slice(&size.to_le_bytes());
        let mut l = [0u8; 16];
        l[..label.len()].copy_from_slice(label.as_bytes());
        e.extend_from_slice(&l);
        e.extend_from_slice(&[0; 4]);
        e
    }

    #[test]
    fn finds_the_coredump_partition_and_ignores_an_erased_one() {
        let mut flash = vec![0xFFu8; 0x300000];
        let mut table = entry(0, 0, 0x10000, 0x200000, "factory");
        table.extend(entry(1, 3, 0x210000, 0x40000, "coredump"));
        table.extend([0xEB, 0xEB]);
        flash[0x8000..0x8000 + table.len()].copy_from_slice(&table);
        let parts = partitions(&flash);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1].label, "coredump");
        assert!(coredump(&flash).is_none(), "erased partition holds no dump");
        flash[0x210000..0x210004].copy_from_slice(&0x1dc0u32.to_le_bytes());
        assert_eq!(coredump(&flash).unwrap().len(), 0x40000);
    }
}
