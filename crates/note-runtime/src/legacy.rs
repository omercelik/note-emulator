//! Read-only import of the old `.emulator/devices.json` (Spec §10).
//! The source file is never modified. Simulator entries and missing images
//! are reported for review; hardware entries are mapped.

use std::path::Path;

use serde_json::Value;

use crate::store::{AvdConfig, Store};
use note_core::{flash, Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accepted {
    pub name: String,
    pub image: String,
    pub network: Option<String>,
    pub battery: Option<Value>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    pub name: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub struct ImportPlan {
    pub accept: Vec<Accepted>,
    pub review: Vec<Review>,
}

const KNOWN: &[&str] = &["id", "name", "image", "backend", "note4c", "network", "battery", "pad_to"];

pub fn plan(text: &str) -> std::result::Result<ImportPlan, String> {
    let entries: Vec<Value> = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut plan = ImportPlan::default();
    for (i, entry) in entries.iter().enumerate() {
        let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed").to_string();
        let obj = entry.as_object().ok_or_else(|| format!("entry {i} is not an object"))?;
        if obj.get("backend").and_then(|v| v.as_str()) == Some("esp-emu") || obj.get("note4c").and_then(|v| v.as_bool()) == Some(false) {
            plan.review.push(Review { name, reason: "simulator entry is not a hardware device".into() });
            continue;
        }
        let Some(image) = obj.get("image").and_then(|v| v.as_str()) else {
            plan.review.push(Review { name, reason: "missing firmware image".into() });
            continue;
        };
        if !Path::new(image).exists() {
            plan.review.push(Review { name, reason: format!("firmware file missing: {image}") });
            continue;
        }
        let mut notes = Vec::new();
        for key in obj.keys() {
            if !KNOWN.contains(&key.as_str()) {
                notes.push(format!("unsupported field {key}"));
            }
        }
        if obj.get("pad_to").is_some() {
            notes.push("pad_to is ignored; the image is imported through the layout checker".into());
        }
        plan.accept.push(Accepted {
            name,
            image: image.to_string(),
            network: obj.get("network").and_then(|v| v.as_str()).map(str::to_string),
            battery: obj.get("battery").cloned(),
            notes,
        });
    }
    Ok(plan)
}

/// Create AVDs for the accepted entries. A layout failure moves that entry to
/// review and leaves the others. `source_text` is not written back.
pub fn import(store: &Store, text: &str, profile: &str, flash_size: usize) -> Result<ImportPlan> {
    let mut plan = plan(text).map_err(Error::BadRequest)?;
    let mut still = Vec::new();
    for item in plan.accept.drain(..) {
        let path = Path::new(&item.image);
        match flash::load(path, flash_size) {
            Ok(image) => {
                let id = store.create(profile, &item.name, &image.bytes, path)?;
                let mut config = store.config(&id)?;
                config.network = item.network.clone();
                config.legacy_battery = item.battery.clone();
                store.set_config(&config)?;
                still.push(item);
            }
            Err(err) => plan.review.push(Review { name: item.name, reason: err.to_string() }),
        }
    }
    plan.accept = still;
    Ok(plan)
}

pub fn config_of(store: &Store, id: &str) -> Result<AvdConfig> {
    store.config(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn hardware_entries_map_simulators_are_reviewed_and_the_file_is_unchanged() {
        let root = std::env::temp_dir().join(format!("note-import-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let image = root.join("fw.bin");
        let mut bytes = vec![0u8; 0x9000];
        bytes[0] = 0xE9;
        bytes[0x8000] = 0xAA;
        bytes[0x8001] = 0x50;
        fs::write(&image, &bytes).unwrap();
        let missing = root.join("nope.bin");
        let doc = format!(
            r#"[
                {{"id":"a","name":"Factory","image":"{}","backend":"qemu","note4c":true,"network":"user","battery":{{"percent":80,"state":"discharging"}},"audio_wav":"x.wav"}},
                {{"id":"b","name":"Simulator","image":"{}","backend":"esp-emu","note4c":false}},
                {{"id":"c","name":"Gone","image":"{}","backend":"qemu","note4c":true}}
            ]"#,
            image.display(),
            image.display(),
            missing.display()
        );
        let before = doc.clone();
        let plan = plan(&doc).unwrap();
        assert_eq!(plan.accept.len(), 1);
        assert_eq!(plan.accept[0].network.as_deref(), Some("user"));
        assert!(plan.accept[0].notes.iter().any(|n| n.contains("audio_wav")));
        assert_eq!(plan.review.len(), 2);
        assert!(plan.review.iter().any(|r| r.reason.contains("simulator")));
        assert!(plan.review.iter().any(|r| r.reason.contains("missing")));
        assert_eq!(doc, before);

        let store = Store::new(root.join("data"));
        let imported = import(&store, &doc, "note4", 0x9000).unwrap();
        assert_eq!(imported.accept.len(), 1);
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].config.network.as_deref(), Some("user"));
        assert!(listed[0].config.legacy_battery.is_some());
        assert_eq!(store.flash(&listed[0].config.id).unwrap(), bytes);
    }
}
