//! Which parts of the model print what they do. One switch, `--debug wifi,spi,net` or
//! `ESP_EMU_DEBUG=wifi,spi,net`; an area is a device name (or its prefix: `spi` covers SPI0/1/2),
//! or one of `rom` (image loading), `net` (virtual network and NAT), `wifi-frames` (802.11
//! frames), `aes` (AES DMA), `mmio` (every register access), `rt` (real-time pacing).
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DebugFlags { areas: BTreeSet<String> }

impl DebugFlags {
    /// `ESP_EMU_DEBUG=a,b,c`, plus the older one-variable-per-area form (`ESP_EMU_DEBUG_SPI`, ...).
    pub fn from_env() -> Self {
        // wasm32-unknown-unknown has no environment: `std::env::vars()` there used to yield nothing
        // and, since Rust 1.9x's std, aborts with "not supported on this platform" — which took
        // every browser demo down the first time Pages built with that toolchain.
        #[cfg(target_arch = "wasm32")]
        { DebugFlags::default() }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut f = DebugFlags::default();
            if let Ok(v) = std::env::var("ESP_EMU_DEBUG") { if v.is_empty() || v == "1" { f.add("rom"); } else { f.parse(&v); } }
            for (k, _) in std::env::vars() {
                if let Some(a) = k.strip_prefix("ESP_EMU_DEBUG_") { f.add(&a.to_ascii_lowercase().replace('_', "-")); }
            }
            if std::env::var("ESP_EMU_RT_LOG").is_ok() { f.add("rt"); }
            if std::env::var("ESP_EMU_LOG_ALL").is_ok() { f.add("mmio"); }
            f
        }
    }
    pub fn parse(&mut self, list: &str) { for a in list.split([',', ' ']).filter(|a| !a.is_empty()) { self.add(a); } }
    pub fn add(&mut self, area: &str) { self.areas.insert(area.to_ascii_lowercase()); }
    pub fn has(&self, area: &str) -> bool { self.areas.contains(area) || self.areas.contains("all") }
    pub fn is_empty(&self) -> bool { self.areas.is_empty() }
    pub fn iter(&self) -> impl Iterator<Item = &str> { self.areas.iter().map(|s| s.as_str()) }
}
