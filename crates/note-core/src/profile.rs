//! Hardware profiles (Spec §3): what a device *is*. Behaviour lives in the
//! board and panel models; a profile only selects and parameterises them.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub const PROFILE_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub schema: u32,
    pub id: String,
    pub name: String,
    pub revision: String,
    #[serde(default)]
    pub provenance: Vec<String>,
    pub soc: String,
    pub flash_mib: u32,
    pub psram: Psram,
    pub display: Display,
    pub buttons: Vec<Button>,
    #[serde(default)]
    pub leds: Vec<Led>,
    pub power: Power,
    pub i2c: I2c,
    pub audio: Audio,
    pub skin: Skin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Psram {
    pub mib: u32,
    pub mode: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelKind {
    /// Solomon SSD2683: the controller on both NOTE4 and NOTE4C (ADR-013).
    Ssd2683,
}

/// The e-paper glass behind the controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelVariant {
    /// NOTE4C: black/white/yellow/red, one colour per 2-bit RAM code.
    Bwry,
    /// NOTE4: black/white, with partial refresh and reference 16-gray waveforms.
    Mono,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DisplayFormat {
    /// Packed 2-bit palette indices, high bits first.
    Pal2,
    /// Packed 4-bit gray levels, high nibble = left pixel, 0 black .. 15 white.
    Gray4,
}

impl DisplayFormat {
    pub fn bits_per_pixel(self) -> u32 {
        match self {
            DisplayFormat::Pal2 => 2,
            DisplayFormat::Gray4 => 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Display {
    pub panel: PanelKind,
    pub variant: PanelVariant,
    pub width: u32,
    pub height: u32,
    pub format: DisplayFormat,
    #[serde(default)]
    pub palette: Vec<String>,
    #[serde(default)]
    pub palette_names: Vec<String>,
    #[serde(default)]
    pub gray_levels: Option<u32>,
    pub spi_host: u8,
    pub pins: PanelPins,
    #[serde(default)]
    pub readback_on_mosi: bool,
}

impl Display {
    /// Size in bytes of one canonical full frame.
    pub fn frame_bytes(&self) -> usize {
        (self.width * self.height * self.format.bits_per_pixel() / 8) as usize
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelPins {
    pub power: u8,
    pub busy: u8,
    pub reset: u8,
    pub dc: u8,
    pub cs: u8,
    pub sclk: u8,
    pub mosi: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Button {
    pub id: String,
    pub gpio: u8,
    pub active_low: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Led {
    pub id: String,
    pub gpio: u8,
    pub active_low: bool,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Power {
    pub battery_latch_gpio: u8,
    pub battery_adc: BatteryAdc,
    pub charge_detect: ChargePin,
    pub charge_full: ChargeFullPin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatteryAdc {
    pub gpio: u8,
    pub unit: u8,
    pub channel: u8,
    pub divider: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChargePin {
    pub gpio: u8,
    pub charging_level: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChargeFullPin {
    pub gpio: u8,
    /// `None` until the polarity is verified for this profile (Spec §3.1).
    pub full_level: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct I2c {
    pub sda: u8,
    pub scl: u8,
    pub devices: Vec<I2cDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct I2cDevice {
    pub kind: String,
    pub address: u8,
    #[serde(default)]
    pub int_gpio: Option<u8>,
    #[serde(default = "default_true")]
    pub supported: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Audio {
    pub i2s: I2sPins,
    pub rail_gpio: u8,
    pub amp_gpio: u8,
    pub sample_rate: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct I2sPins {
    pub mclk: u8,
    pub bclk: u8,
    pub ws: u8,
    pub dout: u8,
    pub din: u8,
}

/// Skin geometry in the skin image's own pixel space (`canvas` × `canvas`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skin {
    pub kind: String,
    pub image: String,
    pub canvas: u32,
    /// The device's extent on the canvas (shell, side keys and shadow); a device window shows
    /// only this. Absent: the whole canvas.
    #[serde(default)]
    pub bounds: Option<Rect>,
    pub screen: Rect,
    #[serde(default)]
    pub hit_areas: Vec<HitArea>,
    #[serde(default)]
    pub leds: Vec<LedSpot>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitArea {
    pub button: String,
    #[serde(flatten)]
    pub shape: HitShape,
    /// `false` when the button-to-position mapping is read off a photo but not yet
    /// confirmed on hardware.
    #[serde(default = "default_true")]
    pub verified: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "lowercase")]
pub enum HitShape {
    Circle { cx: u32, cy: u32, r: u32 },
    Rect { x: u32, y: u32, w: u32, h: u32 },
}

impl HitShape {
    fn bounds(&self) -> (u32, u32) {
        match *self {
            HitShape::Circle { cx, cy, r } => (cx + r, cy + r),
            HitShape::Rect { x, y, w, h } => (x + w, y + h),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedSpot {
    pub led: String,
    pub cx: u32,
    pub cy: u32,
    pub r: u32,
}

impl Profile {
    pub fn load(path: &Path) -> Result<Profile> {
        let invalid = |reason: String| Error::ProfileInvalid { path: path.to_path_buf(), reason };
        let text = fs::read_to_string(path).map_err(|e| Error::io(format!("read {}", path.display()), e))?;
        let profile: Profile = serde_json::from_str(&text).map_err(|e| invalid(e.to_string()))?;
        profile.validate().map_err(invalid)?;
        Ok(profile)
    }

    fn validate(&self) -> std::result::Result<(), String> {
        if self.schema != PROFILE_SCHEMA {
            return Err(format!("schema {} is not supported (expected {PROFILE_SCHEMA})", self.schema));
        }
        if self.soc != "esp32s3" {
            return Err(format!("soc {:?} is not supported", self.soc));
        }
        let d = &self.display;
        if d.width == 0 || d.height == 0 || (d.width * d.format.bits_per_pixel()) % 8 != 0 {
            return Err("display rows must be a whole number of bytes".into());
        }
        match (d.variant, d.format) {
            (PanelVariant::Bwry, DisplayFormat::Pal2) => {
                if d.palette.len() != 4 {
                    return Err("pal2 display needs exactly 4 palette colours".into());
                }
            }
            (PanelVariant::Mono, DisplayFormat::Gray4) => {
                if d.gray_levels != Some(16) {
                    return Err("gray4 display needs gray_levels = 16".into());
                }
            }
            (variant, format) => return Err(format!("panel variant {variant:?} cannot produce format {format:?}")),
        }
        let mut gpios: Vec<u8> = self.buttons.iter().map(|b| b.gpio).collect();
        gpios.extend(self.leds.iter().map(|l| l.gpio));
        gpios.sort_unstable();
        if gpios.windows(2).any(|w| w[0] == w[1]) {
            return Err("a GPIO is assigned to more than one button/LED".into());
        }
        if gpios.iter().any(|&g| g > 48) {
            return Err("ESP32-S3 GPIO numbers must be 0..=48".into());
        }
        let skin = &self.skin;
        let s = skin.screen;
        if s.x + s.w > skin.canvas || s.y + s.h > skin.canvas {
            return Err("skin screen lies outside the canvas".into());
        }
        // The screen window must keep the panel's aspect ratio within 1 %.
        let panel = d.width as f64 / d.height as f64;
        if ((s.w as f64 / s.h as f64) / panel - 1.0).abs() > 0.01 {
            return Err("skin screen aspect ratio does not match the panel".into());
        }
        for hit in &skin.hit_areas {
            if !self.buttons.iter().any(|b| b.id == hit.button) {
                return Err(format!("skin hit area for unknown button {:?}", hit.button));
            }
            let (right, bottom) = hit.shape.bounds();
            if right > skin.canvas || bottom > skin.canvas {
                return Err(format!("skin hit area {:?} lies outside the canvas", hit.button));
            }
        }
        for spot in &skin.leds {
            if !self.leds.iter().any(|l| l.id == spot.led) {
                return Err(format!("skin LED spot for unknown LED {:?}", spot.led));
            }
        }
        Ok(())
    }
}

/// Directory containing the profiles: `NOTE_EMU_PROFILES`; else `Contents/Resources/Profiles`
/// next to an executable inside `NOTE Emulator.app`; else the first `Profiles/` with a
/// `note4.json` above the executable or the working directory (development checkouts, tests).
/// Found at run time, so no build path is compiled into release binaries (PKG-01 audit).
pub fn profiles_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("NOTE_EMU_PROFILES") {
        return PathBuf::from(dir);
    }
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    if let Some(dir) = &exe_dir {
        let bundled = dir.join("../Resources/Profiles");
        if bundled.join("note4.json").is_file() {
            return bundled;
        }
    }
    let starts = exe_dir.into_iter().chain(std::env::current_dir().ok());
    for start in starts {
        for dir in start.ancestors().take(8) {
            let candidate = dir.join("Profiles");
            if candidate.join("note4.json").is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from("Profiles")
}

pub fn load_all(dir: &Path) -> Result<Vec<Profile>> {
    let entries = fs::read_dir(dir).map_err(|e| Error::io(format!("read {}", dir.display()), e))?;
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths.iter().map(|p| Profile::load(p)).collect()
}

pub fn find(dir: &Path, id: &str) -> Result<Profile> {
    load_all(dir)?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| Error::ProfileUnknown(id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_profiles_load_and_differ_only_where_the_hardware_does() {
        let profiles = load_all(&profiles_dir()).expect("profiles load");
        let ids: Vec<&str> = profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["note4", "note4c"]);
        let note4 = &profiles[0];
        let note4c = &profiles[1];

        assert_eq!(note4c.display.panel, PanelKind::Ssd2683);
        assert_eq!(note4c.display.variant, PanelVariant::Bwry);
        assert_eq!(note4c.display.frame_bytes(), 30_000);
        assert_eq!(note4.display.panel, PanelKind::Ssd2683);
        assert_eq!(note4.display.variant, PanelVariant::Mono);
        assert_eq!(note4.display.frame_bytes(), 60_000);
        assert!(note4.display.readback_on_mosi);

        // Shared board wiring (reference/wiring.md).
        for p in &profiles {
            let pins = |id: &str| p.buttons.iter().find(|b| b.id == id).unwrap().gpio;
            assert_eq!((pins("ok"), pins("up"), pins("down")), (0, 39, 18));
            assert_eq!(p.power.battery_latch_gpio, 17);
            assert_eq!((p.power.battery_adc.gpio, p.power.battery_adc.channel), (4, 3));
        }
        // Both have the green status LED on GPIO3, active low, in the same spot on the shell.
        for p in [&note4, &note4c] {
            assert_eq!((p.leds[0].gpio, p.leds[0].active_low), (3, true));
            assert_eq!(p.skin.leds[0].led, "power");
        }
        // NOTE4C: the LED shines through the rightmost hole of the speaker grille's middle row
        // (seen on hardware: it lights there while geminilive's talk button is held).
        let spot = &note4c.skin.leds[0];
        assert_eq!((spot.cx, spot.cy, spot.r), (307, 879, 4));
        assert_eq!((note4.skin.leds[0].cx, note4.skin.leds[0].cy), (776, 879));

        // Every button can be pressed on the skin, and every skin file exists.
        for p in &profiles {
            for b in &p.buttons {
                assert!(p.skin.hit_areas.iter().any(|h| h.button == b.id), "{}: {}", p.id, b.id);
            }
            assert!(profiles_dir().join(&p.skin.image).is_file(), "{}", p.skin.image);
        }
    }

    #[test]
    fn mismatched_panel_and_format_is_rejected() {
        let path = profiles_dir().join("note4c.json");
        let mut value: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        value["display"]["format"] = "gray4".into();
        let bad: Profile = serde_json::from_value(value).unwrap();
        assert!(bad.validate().unwrap_err().contains("cannot produce"));
    }
}
