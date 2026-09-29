//! ESP32-S3 SAR ADC one-shot conversions through the RTC controller (SENS block at
//! RTC_CNTL + 0x800), the path ESP-IDF's `adc_oneshot` driver uses. The driver selects one pad
//! in `SARn_EN_PAD`, pulses `MEASn_START_SAR` 0 → 1 and polls `MEASn_DONE_SAR`, then reads
//! `MEASn_DATA_SAR` (sens_reg.h: MEAS1_CTRL2 +0x0C, MEAS2_CTRL2 +0x30, ATTEN1 +0x14,
//! ATTEN2 +0x38). The analog input comes from the board; the conversion is an ideal 12-bit
//! transfer with the typical full-scale voltage of each attenuation.
//! (NOTE: added by the NOTE Emulator fork — engine/esp32sim/PATCHES.md #3.)

/// Offsets inside the RTC_CNTL device.
pub const MEAS1_CTRL2: u32 = 0x800 + 0x0c;
pub const ATTEN1: u32 = 0x800 + 0x14;
pub const MEAS2_CTRL2: u32 = 0x800 + 0x30;
pub const ATTEN2: u32 = 0x800 + 0x38;

const DONE: u32 = 1 << 16;
const START: u32 = 1 << 17;
const DATA_MASK: u32 = 0xffff;
const EN_PAD_SHIFT: u32 = 19;
const EN_PAD_MASK: u32 = 0xfff;

/// Typical full-scale input in mV for attenuation 0 dB, 2.5 dB, 6 dB, 12 dB.
pub const FULL_SCALE_MV: [u32; 4] = [950, 1250, 1750, 3100];

/// A conversion the bus must complete with the board's pin voltage.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SarRequest {
    /// 1 or 2 (ADC1 / ADC2).
    pub unit: u8,
    pub channel: u8,
    pub atten: u8,
}

/// Ideal 12-bit result for `mv` at the pad.
pub fn raw_for_millivolts(mv: u32, atten: u8) -> u32 {
    let fs = FULL_SCALE_MV[(atten & 3) as usize];
    (mv.min(fs) * 4095 + fs / 2) / fs
}

/// ESP-IDF curve-fitting error coefficients for the S3 (esp_adc/esp32s3/curve_fitting_coefficients.c):
/// per unit and attenuation, up to five (numerator, divisor) terms and their signs.
const ADC1_COEF: [[(u64, u64); 5]; 4] = [
    [(27856531419538344, 10_000_000_000_000_000), (50871540569528, 10_000_000_000_000_000), (9798249589, 1_000_000_000_000_000), (0, 0), (0, 0)],
    [(29831022915028695, 10_000_000_000_000_000), (49393185868806, 10_000_000_000_000_000), (101379430548, 10_000_000_000_000_000), (0, 0), (0, 0)],
    [(23285545746296417, 10_000_000_000_000_000), (147640181047414, 10_000_000_000_000_000), (208385525314, 10_000_000_000_000_000), (0, 0), (0, 0)],
    [(644403418269478, 1_000_000_000_000_000), (644334888647536, 10_000_000_000_000_000), (1297891447611, 10_000_000_000_000_000), (70769718, 1_000_000_000_000_000), (13515, 1_000_000_000_000_000)],
];
const ADC2_COEF: [[(u64, u64); 5]; 4] = [
    [(25668651654328927, 10_000_000_000_000_000), (1353548869615, 10_000_000_000_000_000), (36615265189, 10_000_000_000_000_000), (0, 0), (0, 0)],
    [(23690184690298404, 10_000_000_000_000_000), (66319894226185, 10_000_000_000_000_000), (118964995959, 10_000_000_000_000_000), (0, 0), (0, 0)],
    [(9452499397020617, 10_000_000_000_000_000), (200996773954387, 10_000_000_000_000_000), (259011467956, 10_000_000_000_000_000), (0, 0), (0, 0)],
    [(12247719764336924, 10_000_000_000_000_000), (755717904943462, 10_000_000_000_000_000), (1478791187119, 10_000_000_000_000_000), (79672528, 1_000_000_000_000_000), (15038, 1_000_000_000_000_000)],
];
const ADC1_SIGN: [[i32; 5]; 4] = [[-1, -1, 1, 0, 0], [-1, -1, 1, 0, 0], [-1, -1, 1, 0, 0], [-1, -1, 1, -1, 1]];
const ADC2_SIGN: [[i32; 5]; 4] = [[-1, 1, 1, 0, 0], [-1, -1, 1, 0, 0], [-1, -1, 1, 0, 0], [1, -1, 1, -1, 1]];

/// The millivolts ESP-IDF's `adc_cali_raw_to_voltage` (curve fitting, calibration V1) reports
/// for `raw`, with the same integer arithmetic (esp_adc/adc_cali_curve_fitting.c).
pub fn idf_calibrated_millivolts(raw: u32, unit: u8, atten: u8, digi: u32) -> i32 {
    let a = (atten & 3) as usize;
    let coeff_a = 65536u64 * 850 / digi as u64;
    let v = raw as u64 * coeff_a / 65536;
    if v == 0 { return 0; }
    let (coef, sign) = if unit == 1 { (&ADC1_COEF[a], &ADC1_SIGN[a]) } else { (&ADC2_COEF[a], &ADC2_SIGN[a]) };
    let terms = if a == 3 { 5 } else { 3 };
    let mut variable = 1u64;
    let mut error = (coef[0].0 / coef[0].1) as i32 * sign[0];
    for i in 1..terms {
        variable = variable.wrapping_mul(v);
        error += (variable.wrapping_mul(coef[i].0) / coef[i].1) as i32 * sign[i];
    }
    v as i32 - error
}

/// The 12-bit code for `mv` at the pad. With eFuse calibration (`digi`), the code whose
/// calibrated reading is closest to `mv`, so firmware reads back what the board applied;
/// without it, the ideal transfer.
pub fn raw_for_millivolts_calibrated(mv: u32, unit: u8, atten: u8, digi: Option<u32>) -> u32 {
    let Some(digi) = digi else { return raw_for_millivolts(mv, atten) };
    (0..=4095u32).min_by_key(|&raw| (idf_calibrated_millivolts(raw, unit, atten, digi) - mv as i32).unsigned_abs()).unwrap_or(0)
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct SarAdc {
    pending: Option<SarRequest>,
    pub conversions: u64,
}

impl SarAdc {
    /// A write to `MEASn_CTRL2`: returns the value to store. Read-only DONE/DATA keep the
    /// previous result; a START rising edge queues a conversion and clears DONE.
    pub fn write_ctrl2(&mut self, off: u32, old: u32, v: u32, atten_reg: u32) -> u32 {
        let unit = if off == MEAS1_CTRL2 { 1 } else { 2 };
        let mut stored = (v & !(DONE | DATA_MASK)) | (old & (DONE | DATA_MASK));
        if v & START != 0 && old & START == 0 {
            stored &= !DONE;
            let pads = (v >> EN_PAD_SHIFT) & EN_PAD_MASK;
            let channel = if pads == 0 { 0 } else { pads.trailing_zeros() as u8 };
            let atten = ((atten_reg >> (2 * channel as u32)) & 3) as u8;
            self.pending = Some(SarRequest { unit, channel, atten });
        }
        stored
    }

    pub fn take_request(&mut self) -> Option<SarRequest> {
        self.pending.take()
    }

    /// The value `MEASn_CTRL2` holds after a completed conversion.
    pub fn completed(&mut self, ctrl2: u32, raw: u32) -> u32 {
        self.conversions += 1;
        (ctrl2 & !DATA_MASK) | DONE | (raw & 0xfff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_edge_requests_the_selected_pad_and_attenuation() {
        let mut sar = SarAdc::default();
        let atten = 3 << (2 * 3); // channel 3 at 12 dB
        let ctrl = (1 << 31) | (1 << 18) | ((1 << 3) << EN_PAD_SHIFT);
        let stored = sar.write_ctrl2(MEAS1_CTRL2, DONE | 0x123, ctrl, atten);
        assert_eq!(stored & (DONE | DATA_MASK), DONE | 0x123, "no start yet: result kept");
        assert!(sar.take_request().is_none());
        let stored = sar.write_ctrl2(MEAS1_CTRL2, stored, ctrl | START, atten);
        assert_eq!(stored & DONE, 0, "a new conversion clears DONE");
        assert_eq!(sar.take_request(), Some(SarRequest { unit: 1, channel: 3, atten: 3 }));
        let done = sar.completed(stored, raw_for_millivolts(1950, 3));
        assert_eq!(done & DONE, DONE);
        assert_eq!(done & DATA_MASK, 2576);
    }

    #[test]
    fn calibrated_conversion_reads_back_the_applied_voltage() {
        // Blank calibration V1 eFuse (all diffs 0): ADC1 12 dB reference Dout = 900 at 850 mV.
        for mv in [1700, 1850, 1950, 2050, 2100] {
            let raw = raw_for_millivolts_calibrated(mv, 1, 3, Some(900));
            let back = idf_calibrated_millivolts(raw, 1, 3, 900);
            assert!((back - mv as i32).abs() <= 2, "{mv} mV -> raw {raw} -> {back} mV");
        }
    }

    #[test]
    fn transfer_clips_at_full_scale() {
        assert_eq!(raw_for_millivolts(0, 0), 0);
        assert_eq!(raw_for_millivolts(950, 0), 4095);
        assert_eq!(raw_for_millivolts(5000, 3), 4095);
    }
}
