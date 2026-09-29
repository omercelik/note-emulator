use crate::device::{Device, WriteEffect};
use crate::regram::RegRam;

// ------------------------------------------------------------------ efuse
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Efuse { pub ram: RegRam }
impl Efuse {
    pub fn new(mac: [u8; 6]) -> Self {
        let mut e = Efuse { ram: RegRam::new() };
        e.ram.write(0x44, u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]]));
        e.ram.write(0x48, ((mac[0] as u32) << 8 | mac[1] as u32) | (2 << 18));   // wafer_version_minor_lo = 2 (rev v0.2)
        e.ram.write(0x6c, 1);                                                    // blk_version_major = 1
        e.ram.write(0x1cc, 0x8c);
        e.ram.write(0x1d0, 0);
        e
    }
    pub fn read(&self, off: u32) -> u32 { self.ram.read(off) }
    /// Bits `[bit, bit + len)` of an eFuse block whose read registers start at `base`.
    fn field(&self, base: u32, bit: u32, len: u32) -> u32 {
        let mut v = 0u64;
        for i in 0..2 { v |= (self.ram.read(base + 4 * (bit / 32 + i)) as u64) << (32 * i); }
        ((v >> (bit % 32)) & ((1u64 << len) - 1)) as u32
    }
    /// ADC calibration reference (Dout at 850 mV) for `unit` 1/2 and `atten` 0..3, as ESP-IDF's
    /// esp_efuse_rtc_calib_get_cal_voltage derives it (calibration V1 only; None when
    /// BLK_VERSION_MAJOR != 1). Field positions: efuse/esp32s3/esp_efuse_table.c.
    /// (NOTE fork, engine/esp32sim/PATCHES.md #5.)
    pub fn adc_cal_digi(&self, unit: u8, atten: u8) -> Option<u32> {
        const BLK1: u32 = 0x44;   // EFUSE_RD_MAC_SPI_SYS_0_REG
        const BLK2: u32 = 0x5c;   // EFUSE_RD_SYS_PART1_DATA0_REG
        if self.field(BLK2, 128, 2) != 1 { return None; }
        let d = |bit, len| self.field(BLK2, bit, len) as i64;
        let diff = [d(201, 8), d(209, 8), d(217, 8), d(225, 8),
                    d(233, 8), d(241, 7), d(248, 7), self.field(BLK1, 186, 6) as i64];
        let mut adc1 = [0i64; 4];
        adc1[3] = diff[3] + 900;
        adc1[2] = diff[2] + adc1[3] + 800;
        adc1[1] = diff[1] + adc1[2] + 700;
        adc1[0] = diff[0] + adc1[1] + 800;
        let a = (atten & 3) as usize;
        let digi = if unit == 1 { adc1[a] } else {
            let adc2 = [adc1[0] - diff[4] + 40, adc1[1] - diff[5] + 10, adc1[2] - diff[6] + 20, adc1[3] - diff[7] + 15];
            adc2[a]
        };
        (digi > 0).then_some(digi as u32)
    }
    pub fn write(&mut self, off: u32, v: u32) { match off { 0x1d4 => {} /* CMD: read/pgm done immediately */ _ => self.ram.write(off, v) } }
}

impl Device for Efuse {
    fn read(&mut self, off: u32) -> u32 { Efuse::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { Efuse::write(self, off, v); WriteEffect::NONE }
}

#[cfg(test)]
mod adc_cal_tests {
    use super::*;
    #[test]
    fn default_efuse_is_calibration_v1_with_reference_900_at_12db() {
        let e = Efuse::new([0; 6]);
        assert_eq!(e.adc_cal_digi(1, 3), Some(900));
        assert_eq!(e.adc_cal_digi(1, 0), Some(900 + 800 + 700 + 800));
        assert_eq!(e.adc_cal_digi(2, 3), Some(915));
        let mut blank = Efuse::new([0; 6]);
        blank.ram.write(0x6c, 0);
        assert_eq!(blank.adc_cal_digi(1, 3), None);
    }
}
