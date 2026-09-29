//! Everest ES8311 audio codec on I2C0 (0x18): a register file the guest programs, shared with
//! the board so host playback follows the DAC volume and mute (AUDIO-01). The chip keeps its
//! registers across an ESP reset (it is a separate part on the audio rail). Chip ID 0x8311 at
//! 0xFD/0xFE. Register semantics from esp_codec_dev's es8311 driver: REG32 DAC volume, 0.5 dB
//! steps from -95.5 dB (0x00) to +32 dB (0xFF), 0 dB at 0xBF; REG31 bits 5-6 set = DAC muted.

use std::sync::{Arc, Mutex};

use esp_periph::i2c::I2cDevice;

const DAC_MUTE: usize = 0x31;
const DAC_VOLUME: usize = 0x32;

#[derive(serde::Serialize, serde::Deserialize)]
struct Regs {
    #[serde(with = "emu_core::snap::arr")]
    regs: [u8; 256],
    ptr: u8,
    first: bool,
}

#[derive(Clone)]
pub struct Es8311 {
    inner: Arc<Mutex<Regs>>,
}

// The board's handle is the same chip as the I2C bus's; the bus copy snapshots it.
emu_core::keep_on_restore!(Es8311);

impl Default for Es8311 {
    fn default() -> Self {
        Self::new()
    }
}

impl Es8311 {
    pub fn new() -> Es8311 {
        let mut regs = [0u8; 256];
        regs[0xfd] = 0x83;
        regs[0xfe] = 0x11;
        Es8311 { inner: Arc::new(Mutex::new(Regs { regs, ptr: 0, first: true })) }
    }

    pub fn reg(&self, r: u8) -> u8 {
        self.inner.lock().unwrap().regs[r as usize]
    }

    /// Linear DAC gain the guest programmed: 0 when muted, else 10^(dB/20).
    pub fn dac_gain(&self) -> f32 {
        let g = self.inner.lock().unwrap();
        if g.regs[DAC_MUTE] & 0x60 == 0x60 {
            return 0.0;
        }
        let db = -95.5 + 0.5 * g.regs[DAC_VOLUME] as f32;
        10f32.powf(db / 20.0)
    }
}

impl I2cDevice for Es8311 {
    fn start(&mut self, read: bool) -> bool {
        if !read {
            self.inner.lock().unwrap().first = true;
        }
        true
    }
    fn write(&mut self, b: u8) -> bool {
        let mut g = self.inner.lock().unwrap();
        if g.first {
            g.ptr = b;
            g.first = false;
        } else {
            let p = g.ptr as usize;
            if p != 0xfd && p != 0xfe {
                g.regs[p] = b;
            }
            g.ptr = g.ptr.wrapping_add(1);
        }
        true
    }
    fn read(&mut self) -> u8 {
        let mut g = self.inner.lock().unwrap();
        let v = g.regs[g.ptr as usize];
        g.ptr = g.ptr.wrapping_add(1);
        v
    }
    fn save(&self) -> Vec<u8> {
        emu_core::snap::to_bytes(&*self.inner.lock().unwrap()).unwrap_or_default()
    }
    fn restore(&mut self, bytes: &[u8]) -> Result<(), String> {
        let r: Regs = emu_core::snap::from_bytes(bytes).map_err(|e| format!("es8311: {e}"))?;
        *self.inner.lock().unwrap() = r;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(c: &mut Es8311, reg: u8, v: u8) {
        c.start(false);
        c.write(reg);
        c.write(v);
        c.stop();
    }

    #[test]
    fn volume_and_mute_follow_the_driver_encoding() {
        let mut c = Es8311::new();
        assert_eq!((c.reg(0xfd), c.reg(0xfe)), (0x83, 0x11));
        write(&mut c, 0x32, 0xbf);
        assert!((c.dac_gain() - 1.0).abs() < 1e-6, "0xBF is 0 dB");
        write(&mut c, 0x32, 0xbf - 12);
        assert!((c.dac_gain() - 0.5012).abs() < 1e-3, "-6 dB");
        write(&mut c, 0x31, 0x60);
        assert_eq!(c.dac_gain(), 0.0, "muted");
        let shared = c.clone();
        write(&mut c, 0x31, 0x00);
        assert!(shared.dac_gain() > 0.0, "one chip behind every handle");
        write(&mut c, 0xfd, 0);
        assert_eq!(c.reg(0xfd), 0x83, "chip ID is read-only");
    }
}
