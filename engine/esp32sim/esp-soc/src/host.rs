//! The few host services the SoC needs that differ between a native process and a browser.
//! Natively they come from `std`; in the wasm build the JS glue supplies them before each run slice.

#[cfg(target_arch = "wasm32")]
static UNIX_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Wall-clock time as milliseconds since the Unix epoch (what the emulated SNTP server hands out).
pub fn unix_time_ms() -> u64 {
    #[cfg(target_arch = "wasm32")]
    { UNIX_MS.load(std::sync::atomic::Ordering::Relaxed) }
    #[cfg(not(target_arch = "wasm32"))]
    { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0) }
}

/// Wasm glue: publish the browser's `Date.now()`.
#[cfg(target_arch = "wasm32")]
pub fn set_unix_time_ms(ms: u64) { UNIX_MS.store(ms, std::sync::atomic::Ordering::Relaxed); }
