use crate::device::{Device, WriteEffect};
use crate::regram::RegRam;
use emu_core::ClockDomain;

// ------------------------------------------------------------------ RTC controller
/// Reset causes (RTC_CNTL_RESET_CAUSE_PROCPU), as the ROM prints them.
pub const RST_POWERON: u32 = 1; pub const RST_SW_SYS: u32 = 3; pub const RST_RTCWDT_SYS: u32 = 9; pub const RST_SW_CPU: u32 = 12;
pub const RST_RTCWDT_CPU: u32 = 13; pub const RST_RTCWDT_RTC: u32 = 16;
pub fn reset_cause_name(c: u32) -> &'static str {
    match c { 1 => "POWERON", 3 => "RTC_SW_SYS_RESET", 5 => "DEEPSLEEP", 7 => "TG0WDT_SYS_RESET", 8 => "TG1WDT_SYS_RESET", 9 => "RTCWDT_SYS_RESET", 11 => "TG0WDT_CPU_RESET",
            12 => "RTC_SW_CPU_RESET", 13 => "RTCWDT_CPU_RESET", 15 => "RTCWDT_BROWN_OUT_RESET", 16 => "RTCWDT_RTC_RESET", 17 => "TG1WDT_CPU_RESET", 18 => "SUPER_WDT_RESET", _ => "?" }
}

/// RTC_CNTL: reset control, slow-clock time and the RTC watchdog.
/// WDTCONFIG0..WDTWPROTECT live at 0x98..0xb0 on S3 and 0x90..0xa8 on C3.
/// `esp_restart()` on ESP-IDF 5.x arms this watchdog and spins until it resets the chip.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct RtcCntl { pub ram: RegRam, pub slow_ticks: u64, pub time_latch: u64, pub sw_reset: bool, pub reset_cause: u32,
                     wdt_base: u32, wdt_count: u64, wdt_stage: usize, wdt_unlocked: bool,
                     /// S3 SAR ADC one-shot path in the SENS block (None on the C3, whose SENS differs).
                     pub sar: Option<crate::sar_adc::SarAdc>,
                     /// S3 light/deep sleep (NOTE fork): `SLEEP_EN` was set and no wake-up happened yet.
                     pub sleeping: bool, sleep_model: bool, wake_cause: u32, deep: bool,
                     /// Completed sleeps (light and deep), for tests and logs.
                     pub sleeps: u64 }

/// RTC_CNTL sleep registers (S3) and wake-up sources (`RTC_*_TRIG_EN`, soc/rtc.h).
const SLP_TIMER0: u32 = 0x4;
const SLP_TIMER1: u32 = 0x8;
const STATE0: u32 = 0x18;
const WAKEUP_STATE: u32 = 0x3c;
const INT_ENA: u32 = 0x40;
const INT_RAW: u32 = 0x44;
const INT_ST: u32 = 0x48;
const INT_CLR: u32 = 0x4c;
const DIG_PWC: u32 = 0x90;
const SLP_REJECT_CAUSE: u32 = 0x128;
const SLP_WAKEUP_CAUSE: u32 = 0x130;
pub const WAKE_GPIO: u32 = 1 << 2;
pub const WAKE_TIMER: u32 = 1 << 3;
const SLP_WAKEUP_INT: u32 = 1 << 0;
const RST_DEEPSLEEP: u32 = 5;
impl RtcCntl {
    pub fn preset_after_bootloader(&mut self) { self.ram.write(0xc0, 0xFFD7_0028); self.ram.write(0xc4, 0xFF0F_00F0); }
    fn request_reset(&mut self, cause: u32) { if !self.sw_reset { self.sw_reset = true; self.reset_cause = cause; } }
    /// Advance the watchdog by RTC slow-clock ticks.
    pub fn wdt_tick(&mut self, ticks: u64) {
        let conf0 = self.ram.read(self.wdt_base);
        if conf0 & (1 << 31) == 0 { return; }
        if self.sleeping && conf0 & (1 << 9) != 0 { return; }                // WDT_PAUSE_IN_SLP
        self.wdt_count += ticks;
        while self.wdt_stage < 4 {
            let mut timeout = self.ram.read(self.wdt_base + 4 + 4 * self.wdt_stage as u32) as u64;
            // Stage 0's hold is scaled by the chip: IDF writes `timeout >> (1 + WDT_DELAY_SEL)`
            // (rwdt_ll_config_stage, eFuse WDT_DELAY_SEL = 0 here), so it counts twice the value.
            if self.wdt_stage == 0 { timeout <<= 1; }
            let action = (conf0 >> (28 - 3 * self.wdt_stage as u32)) & 7;
            if action == 0 { self.wdt_stage += 1; continue; }              // stage disabled: skip
            if self.wdt_count < timeout { break; }
            self.wdt_count = 0; self.wdt_stage += 1;
            match action {
                1 => { self.ram.write(0x44, self.ram.read(0x44) | (1 << 3)); }   // INT_RAW.WDT
                2 => self.request_reset(RST_RTCWDT_CPU),
                3 => self.request_reset(RST_RTCWDT_SYS),
                4 => self.request_reset(RST_RTCWDT_RTC),
                _ => {}
            }
            if self.sw_reset { break; }
        }
        if self.wdt_stage >= 4 { self.wdt_stage = 0; }
    }
    pub fn new() -> Self { let mut r = Self::with_wdt_base(0x98); r.sar = Some(Default::default()); r.sleep_model = true; r }
    pub fn new_c3() -> Self { Self::with_wdt_base(0x90) }
    /// Complete a queued SAR conversion with its 12-bit result.
    pub fn complete_sar(&mut self, unit: u8, raw: u32) {
        let off = if unit == 1 { crate::sar_adc::MEAS1_CTRL2 } else { crate::sar_adc::MEAS2_CTRL2 };
        let old = self.ram.read(off);
        if let Some(sar) = &mut self.sar { let v = sar.completed(old, raw); self.ram.write(off, v); }
    }
    /// Wake-up sources latched when sleep began (`WAKEUP_STATE.WAKEUP_ENA`).
    fn wake_ena(&self) -> u32 { self.ram.read(WAKEUP_STATE) >> 15 }

    /// The RTC domain survives a digital reset: after a deep-sleep wake the application still
    /// reads the wake-up cause (`esp_sleep_get_wakeup_cause`). Called by the SoC's reboot.
    pub fn keep_sleep_state(&mut self, old: &RtcCntl) {
        self.wake_cause = old.wake_cause;
        self.sleeps = old.sleeps;
    }

    /// Asleep and waiting for a digital GPIO level (`gpio_wakeup_enable`); the SoC checks pins.
    pub fn sleep_wants_gpio(&self) -> bool { self.sleeping && self.wake_ena() & WAKE_GPIO != 0 }

    /// End the sleep with `cause` (a `WAKE_*` bit). Light sleep raises `SLP_WAKEUP_INT_RAW`,
    /// which `rtc_sleep_start` spins on; deep sleep resets the chip with cause DEEPSLEEP.
    pub fn wake(&mut self, cause: u32) {
        if !self.sleeping { return; }
        self.sleeping = false;
        self.sleeps += 1;
        self.wake_cause = cause;
        self.ram.write(INT_RAW, self.ram.read(INT_RAW) | SLP_WAKEUP_INT);
        if self.deep { self.request_reset(RST_DEEPSLEEP); }
    }

    fn check_timer_wake(&mut self) {
        if !self.sleeping || self.wake_ena() & WAKE_TIMER == 0 { return; }
        let t1 = self.ram.read(SLP_TIMER1);
        if t1 & (1 << 16) == 0 { return; }                                   // MAIN_TIMER_ALARM_EN
        let target = self.ram.read(SLP_TIMER0) as u64 | ((t1 as u64 & 0xffff) << 32);
        if self.slow_ticks >= target { self.wake(WAKE_TIMER); }
    }

    fn with_wdt_base(wdt_base: u32) -> Self {
        let mut r = RtcCntl { ram: RegRam::new(), slow_ticks: 0, time_latch: 0, sw_reset: false, reset_cause: RST_POWERON, wdt_base, wdt_count: 0, wdt_stage: 0, wdt_unlocked: false, sar: None,
                              sleeping: false, sleep_model: false, wake_cause: 0, deep: false, sleeps: 0 };
        r.ram.write(0x38, 1 | (1 << 6));           // RESET_STATE: reset cause POWERON for both CPUs
        r.ram.write(0x74, 0);                        // CLK_CONF
        r
    }
    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x10 => self.time_latch as u32, 0x14 => (self.time_latch >> 32) as u32,
            0xc => self.ram.read(off) | (1 << 30),  // TIME_UPDATE: valid
            INT_ST if self.sleep_model => self.ram.read(INT_RAW) & self.ram.read(INT_ENA),
            SLP_WAKEUP_CAUSE if self.sleep_model => self.wake_cause,
            SLP_REJECT_CAUSE if self.sleep_model => 0,
            0x1fc => 0x2007270,
            0x850 => (self.ram.read(off) & !0x1ff) | (1 << 8) | 0x80,   // SENS_SAR_TSENS_CTRL (SENS block at +0x800): TSENS_READY, raw ~ room temperature
            _ => self.ram.read(off),
        }
    }
    pub fn write(&mut self, off: u32, v: u32) {
        let wdt = self.wdt_base;
        match off {
            0x0 => { if v & (1 << 31) != 0 { self.request_reset(RST_SW_SYS); } else if v & (1 << 5) != 0 { self.request_reset(RST_SW_CPU); } self.ram.write(off, v & !((1 << 31) | (1 << 5))); }   // OPTIONS0.SW_SYS_RST / SW_PROCPU_RST
            0xc => { if v & (1 << 31) != 0 { self.time_latch = self.slow_ticks; } self.ram.write(off, v); }
            INT_CLR if self.sleep_model => { self.ram.write(INT_RAW, self.ram.read(INT_RAW) & !v); }
            STATE0 if self.sleep_model && v & (1 << 31) != 0 => {
                // SLEEP_EN: the chip sleeps until an enabled source wakes it (NOTE fork).
                self.ram.write(off, v & !(1 << 31));
                self.sleeping = true;
                self.wake_cause = 0;
                self.deep = self.ram.read(DIG_PWC) & (1 << 31) != 0;       // DG_WRAP_PD_EN
                self.check_timer_wake();
            }
            _ if off == wdt + 0x18 => { self.wdt_unlocked = v == 0x50D8_3AA1; self.ram.write(off, v); }
            _ if (wdt..=wdt + 0x10).contains(&off) => { if self.wdt_unlocked { if off == wdt && (v ^ self.ram.read(wdt)) & (1 << 31) != 0 { self.wdt_count = 0; self.wdt_stage = 0; } self.ram.write(off, v); } }
            _ if off == wdt + 0x14 => { if self.wdt_unlocked && v & (1 << 31) != 0 { self.wdt_count = 0; self.wdt_stage = 0; } }   // WDTFEED
            crate::sar_adc::MEAS1_CTRL2 | crate::sar_adc::MEAS2_CTRL2 if self.sar.is_some() => {
                let atten = self.ram.read(if off == crate::sar_adc::MEAS1_CTRL2 { crate::sar_adc::ATTEN1 } else { crate::sar_adc::ATTEN2 });
                let old = self.ram.read(off);
                let stored = self.sar.as_mut().unwrap().write_ctrl2(off, old, v, atten);
                self.ram.write(off, stored);
            }
            _ => self.ram.write(off, v),
        }
    }
}

impl Default for RtcCntl { fn default() -> Self { Self::new() } }

impl Device for RtcCntl {
    fn read(&mut self, off: u32) -> u32 { RtcCntl::read(self, off) }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect { RtcCntl::write(self, off, v); WriteEffect::NONE }
    fn clock(&self) -> Option<ClockDomain> { Some(ClockDomain::RtcSlow) }
    fn tick(&mut self, ticks: u64) { self.slow_ticks += ticks; self.wdt_tick(ticks); self.check_timer_wake(); }
}

#[cfg(test)]
mod sleep_tests {
    use super::*;

    fn arm(r: &mut RtcCntl, wake: u32, deep: bool) {
        r.write(WAKEUP_STATE, wake << 15);
        if deep { r.write(DIG_PWC, 1 << 31); }
    }

    #[test]
    fn timer_wake_raises_the_interrupt_rtc_sleep_start_polls() {
        let mut r = RtcCntl::new();
        arm(&mut r, WAKE_TIMER, false);
        r.write(SLP_TIMER0, 1000);
        r.write(SLP_TIMER1, 1 << 16);
        r.write(INT_CLR, 3);
        r.write(STATE0, 1 << 31);
        assert!(r.sleeping);
        assert_eq!(r.read(INT_RAW) & 3, 0, "not yet");
        Device::tick(&mut r, 999);
        assert!(r.sleeping);
        Device::tick(&mut r, 1);
        assert!(!r.sleeping);
        assert_eq!((r.read(INT_RAW) & 1, r.read(SLP_WAKEUP_CAUSE)), (1, WAKE_TIMER));
        r.write(INT_CLR, 1);
        assert_eq!(r.read(INT_RAW), 0);
        assert!(!r.sw_reset, "light sleep does not reset");
    }

    #[test]
    fn gpio_wake_comes_from_the_soc_and_deep_sleep_resets() {
        let mut r = RtcCntl::new();
        arm(&mut r, WAKE_GPIO, true);
        r.write(STATE0, 1 << 31);
        assert!(r.sleep_wants_gpio());
        Device::tick(&mut r, 1_000_000);
        assert!(r.sleeping, "no timer source");
        r.wake(WAKE_GPIO);
        assert_eq!((r.sw_reset, r.reset_cause, r.read(SLP_WAKEUP_CAUSE)), (true, RST_DEEPSLEEP, WAKE_GPIO));
    }

    #[test]
    fn the_rtc_watchdog_pauses_in_sleep_when_asked() {
        let mut r = RtcCntl::new();
        r.write(0x98 + 0x18, 0x50D8_3AA1);
        r.write(0x98 + 4, 50);                                        // stage 0: 100 ticks
        r.write(0x98, (1 << 31) | (4 << 28) | (1 << 9));             // enable, reset RTC, pause in sleep
        arm(&mut r, WAKE_GPIO, false);
        r.write(STATE0, 1 << 31);
        Device::tick(&mut r, 1000);
        assert!(!r.sw_reset, "asleep: paused");
        r.wake(WAKE_GPIO);
        Device::tick(&mut r, 100);
        assert!(r.sw_reset, "awake: counts again");
    }

    #[test]
    fn the_c3_layout_has_no_sleep_model() {
        let mut r = RtcCntl::new_c3();
        r.write(STATE0, 1 << 31);
        assert!(!r.sleeping);
    }
}
