//! NXP PCF8563 real-time clock on I2C0 (0x51).
//!
//! Time registers are BCD. Two clock policies (RTC-01):
//! - [`ClockPolicy::Fixed`] — a base Unix time plus elapsed virtual seconds.
//!   Emulator pause freezes virtual time, so this clock freezes too.
//! - [`ClockPolicy::Live`] — the host wall clock plus a guest-written offset.
//!   Emulator pause does not freeze it.
//! The chip's STOP bit (control 1, bit 5) freezes either policy and continues
//! from the frozen instant when cleared. Guest reset reattaches this device;
//! the registers live in the shared chip, so they survive an ESP reset.
//! Alarm (and the demo's 1 Hz countdown timer) pull INT, active low, on the
//! profile's RTC GPIO. Other timer frequencies are not modelled: they do not
//! set TF.
//!
//! Snapshot restore is G7 and is not implemented. Defined effect when it lands:
//! Fixed restores `base_unix` together with the virtual cycle counter; Live
//! keeps the offset and reads the host clock again. A restore must not invent
//! alarm flags for minutes the restored clock skipped.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use esp_periph::i2c::I2cDevice;

const CPU_HZ: u64 = 240_000_000;
const STOP: u8 = 1 << 5;
const AF: u8 = 1 << 3;
const TF: u8 = 1 << 2;
const AIE: u8 = 1 << 1;
const TIE: u8 = 1 << 0;
const AE: u8 = 1 << 7;
const TIMER_EN: u8 = 1 << 7;
const TIMER_1HZ: u8 = 0x02;

/// How the RTC turns a moment into the time the guest reads.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockPolicy {
    /// `base_unix` at virtual cycle 0, plus whole virtual seconds.
    Fixed,
    /// Host Unix time plus `live_offset`. Independent of virtual time.
    Live,
}

/// One guest write of the time registers (explicit clock changes are logged).
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockSet {
    pub previous: i64,
    pub written: i64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Inner {
    regs: [u8; 16],
    ptr: u8,
    first: bool,
    wrote_time: bool,
    /// A register write may have changed INT; the board re-samples it at once.
    int_dirty: bool,
    /// The enabled alarm fields matched at the last sample (AF is set on a new match only).
    alarm_matched: bool,
    policy: ClockPolicy,
    /// Fixed: Unix seconds at virtual cycle 0. Unused as a base while Live.
    base_unix: i64,
    /// Live: added to the host clock. Fixed: unused.
    live_offset: i64,
    /// Test seam. `None` reads `SystemTime`.
    live_unix: Option<i64>,
    /// Instant held while STOP is set.
    frozen: Option<i64>,
    /// Last second `catch_up` applied. Discontinuities do not replay the gap.
    last_unix: i64,
    timer_reload: u8,
    /// The board's cycle counter, shared; not snapshot state.
    #[serde(skip, default)]
    cycles: Arc<AtomicU64>,
    sets: Vec<ClockSet>,
}

impl Inner {
    fn elapsed(&self) -> i64 {
        (self.cycles.load(Ordering::Relaxed) / CPU_HZ) as i64
    }

    fn host_unix(&self) -> i64 {
        if let Some(t) = self.live_unix {
            return t;
        }
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    fn running_unix(&self) -> i64 {
        match self.policy {
            ClockPolicy::Fixed => self.base_unix + self.elapsed(),
            ClockPolicy::Live => self.host_unix().saturating_add(self.live_offset),
        }
    }

    fn now_unix(&self) -> i64 {
        if self.regs[0] & STOP != 0 {
            self.frozen.unwrap_or_else(|| self.running_unix())
        } else {
            self.running_unix()
        }
    }

    fn set_policy(&mut self, policy: ClockPolicy) {
        let shown = self.now_unix();
        self.policy = policy;
        match policy {
            ClockPolicy::Fixed => self.base_unix = shown - self.elapsed(),
            ClockPolicy::Live => self.live_offset = shown.saturating_sub(self.host_unix()),
        }
        if self.regs[0] & STOP != 0 {
            self.frozen = Some(shown);
        }
        self.last_unix = shown;
    }

    fn load_time(&mut self) {
        self.catch_up();
        let (y, mo, d, h, mi, s, wd) = civil(self.now_unix());
        self.regs[2] = bcd(s); // VL = 0: clock integrity guaranteed
        self.regs[3] = bcd(mi);
        self.regs[4] = bcd(h);
        self.regs[5] = bcd(d);
        self.regs[6] = wd;
        self.regs[7] = bcd(mo) | if y >= 2100 { 0x80 } else { 0 };
        self.regs[8] = bcd((y % 100) as u8);
    }

    fn store_time(&mut self) {
        let previous = self.now_unix();
        let r = &self.regs;
        let y = 2000 + unbcd(r[8]) as i64 + if r[7] & 0x80 != 0 { 100 } else { 0 };
        let written = days_from_civil(y, unbcd(r[7] & 0x1f) as i64, unbcd(r[5] & 0x3f) as i64)
            * 86_400
            + unbcd(r[4] & 0x3f) as i64 * 3600
            + unbcd(r[3] & 0x7f) as i64 * 60
            + unbcd(r[2] & 0x7f) as i64;
        match self.policy {
            ClockPolicy::Fixed => self.base_unix = written - self.elapsed(),
            ClockPolicy::Live => self.live_offset = written.saturating_sub(self.host_unix()),
        }
        if self.regs[0] & STOP != 0 {
            self.frozen = Some(written);
        }
        // A programmed jump is not a run of seconds. Don't fire the timer for the gap.
        self.last_unix = written;
        if written != previous {
            self.sets.push(ClockSet { previous, written });
        }
    }

    fn on_reg(&mut self, reg: u8, value: u8) {
        match reg {
            0 => {
                let was = self.regs[0] & STOP != 0;
                self.regs[0] = value;
                let now = value & STOP != 0;
                if now && !was {
                    let t = self.running_unix();
                    self.frozen = Some(t);
                    self.last_unix = t;
                } else if !now && was {
                    let frozen = self.frozen.take().unwrap_or_else(|| self.running_unix());
                    match self.policy {
                        ClockPolicy::Fixed => self.base_unix = frozen - self.elapsed(),
                        ClockPolicy::Live => {
                            self.live_offset = frozen.saturating_sub(self.host_unix())
                        }
                    }
                    self.last_unix = frozen;
                }
            }
            0x0f => {
                self.regs[0x0f] = value;
                self.timer_reload = value;
            }
            _ => {
                self.regs[reg as usize] = value;
                self.wrote_time |= (2..=8).contains(&reg);
            }
        }
    }

    fn catch_up(&mut self) {
        if self.regs[0] & STOP != 0 {
            return;
        }
        let now = self.running_unix();
        let last = self.last_unix;
        if now == last {
            self.sample_alarm(now);
            return;
        }
        // A host-clock step (live policy) or a long unpolled gap is one sample,
        // not a replay of every skipped second.
        if now < last || now - last > 86_400 {
            self.last_unix = now;
            self.sample_alarm(now);
            return;
        }
        let mut t = last;
        while t < now {
            t += 1;
            self.sample_alarm(t);
            self.tick_timer();
        }
        self.last_unix = now;
    }

    /// AF is set when the enabled fields *first* match (datasheet 8.5.5): a guest that clears
    /// AF inside the alarm minute does not see it again until the next match.
    fn sample_alarm(&mut self, t: i64) {
        let matched = self.alarm_match(t);
        if matched && !self.alarm_matched {
            self.regs[1] |= AF;
        }
        self.alarm_matched = matched;
    }

    /// Enabled fields (AE clear) must all match. Every field disabled means no alarm.
    fn alarm_match(&self, t: i64) -> bool {
        let (_y, _mo, d, h, mi, _s, wd) = civil(t);
        let fields = [
            (self.regs[9], mi, 0x7f),
            (self.regs[10], h, 0x3f),
            (self.regs[11], d, 0x3f),
            (self.regs[12], wd, 0x07),
        ];
        let mut any = false;
        for (reg, value, mask) in fields {
            if reg & AE != 0 {
                continue;
            }
            any = true;
            if unbcd(reg & mask) != value {
                return false;
            }
        }
        any
    }

    fn tick_timer(&mut self) {
        let ctl = self.regs[0x0e];
        if ctl & TIMER_EN == 0 || ctl & 0x03 != TIMER_1HZ {
            return;
        }
        let reload = self.timer_reload.max(1);
        let mut count = self.regs[0x0f];
        if count == 0 {
            count = reload;
        }
        count -= 1;
        if count == 0 {
            self.regs[1] |= TF;
            count = reload;
        }
        self.regs[0x0f] = count;
    }

    fn int_low(&mut self) -> bool {
        self.int_dirty = false;
        self.catch_up();
        let c = self.regs[1];
        (c & AF != 0 && c & AIE != 0) || (c & TF != 0 && c & TIE != 0)
    }

    fn next_deadline(&self) -> Option<u64> {
        if self.int_dirty {
            return Some(self.cycles.load(Ordering::Relaxed) + 1);
        }
        if self.regs[0] & STOP != 0 {
            return None;
        }
        let armed = self.regs[1] & (AIE | TIE) != 0 || self.regs[0x0e] & TIMER_EN != 0;
        if !armed {
            return None;
        }
        let c = self.cycles.load(Ordering::Relaxed);
        Some(match self.policy {
            ClockPolicy::Fixed => (c / CPU_HZ + 1) * CPU_HZ,
            // Wall time is not the virtual clock. Poll a tenth of a second so a
            // live alarm is noticed without a 1 ms tick storm.
            ClockPolicy::Live => c.saturating_add(CPU_HZ / 10),
        })
    }
}

/// Shared PCF8563. Cloning keeps the same chip across an I2C reattach (guest reset).
#[derive(Clone)]
pub struct Pcf8563 {
    inner: Arc<Mutex<Inner>>,
}

impl Pcf8563 {
    pub fn new(base_unix: i64, cycles: Arc<AtomicU64>) -> Pcf8563 {
        Pcf8563 {
            inner: Arc::new(Mutex::new(Inner {
                regs: [0; 16],
                ptr: 0,
                first: true,
                wrote_time: false,
                int_dirty: false,
                alarm_matched: false,
                policy: ClockPolicy::Fixed,
                base_unix,
                live_offset: 0,
                live_unix: None,
                frozen: None,
                last_unix: base_unix,
                timer_reload: 0,
                cycles,
                sets: Vec::new(),
            })),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        f(&mut self.inner.lock().expect("rtc poisoned"))
    }

    pub fn set_policy(&self, policy: ClockPolicy) {
        self.with(|i| i.set_policy(policy));
    }

    pub fn policy(&self) -> ClockPolicy {
        self.with(|i| i.policy)
    }

    /// Pin the live-policy host clock (tests). Production reads `SystemTime`.
    pub fn set_live_unix(&self, unix: i64) {
        self.with(|i| i.live_unix = Some(unix));
    }

    /// INT pin, active low. Polls the alarm and the 1 Hz timer up to `now`.
    pub fn int_is_low(&self) -> bool {
        self.with(|i| i.int_low())
    }

    pub fn next_deadline(&self) -> Option<u64> {
        self.with(|i| i.next_deadline())
    }

    pub fn clock_sets(&self) -> Vec<ClockSet> {
        self.with(|i| i.sets.clone())
    }
}

// The board's `rtc` handle is the same chip as the I2C bus's; the bus copy snapshots it.
emu_core::keep_on_restore!(Pcf8563);

impl I2cDevice for Pcf8563 {
    fn save(&self) -> Vec<u8> {
        self.with(|i| emu_core::snap::to_bytes(&*i).unwrap_or_default())
    }
    fn restore(&mut self, bytes: &[u8]) -> Result<(), String> {
        let mut state: Inner = emu_core::snap::from_bytes(bytes).map_err(|e| format!("pcf8563: {e}"))?;
        self.with(|i| {
            state.cycles = i.cycles.clone();
            *i = state;
        });
        Ok(())
    }
    fn start(&mut self, read: bool) -> bool {
        self.with(|i| {
            if read {
                i.load_time();
            } else {
                i.first = true;
            }
        });
        true
    }
    fn write(&mut self, b: u8) -> bool {
        self.with(|i| {
            if i.first {
                i.ptr = b & 0x0f;
                i.first = false;
            } else {
                let reg = i.ptr;
                i.on_reg(reg, b);
                i.int_dirty = true;
                i.ptr = (i.ptr + 1) & 0x0f;
            }
        });
        true
    }
    fn read(&mut self) -> u8 {
        self.with(|i| {
            let v = i.regs[i.ptr as usize];
            i.ptr = (i.ptr + 1) & 0x0f;
            v
        })
    }
    fn stop(&mut self) {
        self.with(|i| {
            if std::mem::take(&mut i.wrote_time) {
                i.store_time();
            }
        });
    }
}

fn bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

fn unbcd(v: u8) -> u8 {
    (v >> 4) * 10 + (v & 0x0f)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (year, month, day, hour, minute, second, weekday 0 = Sunday) for Unix seconds.
fn civil(t: i64) -> (i64, u8, u8, u8, u8, u8, u8) {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    let wd = (days + 4).rem_euclid(7) as u8;
    (
        y,
        m,
        d,
        (secs / 3600) as u8,
        (secs / 60 % 60) as u8,
        (secs % 60) as u8,
        wd,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use esp_periph::i2c::I2cDevice;

    fn read_time(rtc: &mut Pcf8563) -> Vec<u8> {
        rtc.start(false);
        rtc.write(0x02);
        rtc.stop();
        rtc.start(true);
        (0..7).map(|_| rtc.read()).collect()
    }

    fn write_clock(rtc: &mut Pcf8563, bytes: &[u8]) {
        rtc.start(false);
        rtc.write(0x02);
        for &b in bytes {
            rtc.write(b);
        }
        rtc.stop();
    }

    #[test]
    fn reports_bcd_time_that_advances_with_virtual_cycles() {
        let cycles = Arc::new(AtomicU64::new(0));
        // 2026-09-27 21:59:58 UTC, a Sunday.
        let mut rtc = Pcf8563::new(1_790_546_398, cycles.clone());
        assert_eq!(read_time(&mut rtc), [0x58, 0x59, 0x21, 0x27, 0, 0x09, 0x26]);
        cycles.store(3 * CPU_HZ, Ordering::Relaxed); // rolls over the hour
        assert_eq!(read_time(&mut rtc), [0x01, 0x00, 0x22, 0x27, 0, 0x09, 0x26]);
    }

    #[test]
    fn guest_writes_set_the_clock() {
        let cycles = Arc::new(AtomicU64::new(10 * CPU_HZ));
        let mut rtc = Pcf8563::new(0, cycles.clone());
        write_clock(&mut rtc, &[0x00, 0x30, 0x12, 0x01, 0x01, 0x01, 0x25]); // 2025-01-01 12:30:00
        cycles.store(15 * CPU_HZ, Ordering::Relaxed);
        assert_eq!(read_time(&mut rtc)[..3], [0x05, 0x30, 0x12]);
        assert_eq!(rtc.clock_sets().len(), 1);
    }

    #[test]
    fn date_rolls_from_new_years_eve_and_a_non_leap_february() {
        let cycles = Arc::new(AtomicU64::new(0));
        let mut rtc = Pcf8563::new(0, cycles.clone());
        write_clock(&mut rtc, &[0x59, 0x59, 0x23, 0x31, 0x04, 0x12, 0x26]); // 2026-12-31
        let before = read_time(&mut rtc)[4];
        cycles.store(2 * CPU_HZ, Ordering::Relaxed);
        let t = read_time(&mut rtc);
        assert_eq!(&t[..4], &[0x01, 0x00, 0x00, 0x01], "2027-01-01 00:00:01");
        assert_eq!(t[5], 0x01);
        assert_eq!(t[6], 0x27);
        assert_ne!(t[4], before, "weekday moves with the date");

        cycles.store(0, Ordering::Relaxed);
        write_clock(&mut rtc, &[0x00, 0x00, 0x00, 0x28, 0x06, 0x02, 0x26]); // 2026-02-28
        cycles.store(CPU_HZ * 86_400, Ordering::Relaxed);
        let march = read_time(&mut rtc);
        assert_eq!(
            (march[3], march[5], march[6]),
            (0x01, 0x03, 0x26),
            "2026 is not a leap year"
        );
    }

    #[test]
    fn stop_bit_freezes_fixed_and_live_clocks() {
        let cycles = Arc::new(AtomicU64::new(0));
        let mut rtc = Pcf8563::new(0, cycles.clone());
        rtc.start(false);
        rtc.write(0x00);
        rtc.write(STOP);
        rtc.stop();
        cycles.store(5 * CPU_HZ, Ordering::Relaxed);
        assert_eq!(read_time(&mut rtc)[0], 0x00, "STOP holds the second");
        rtc.start(false);
        rtc.write(0x00);
        rtc.write(0x00); // release STOP at elapsed = 5 s
        rtc.stop();
        cycles.store(7 * CPU_HZ, Ordering::Relaxed);
        assert_eq!(
            read_time(&mut rtc)[0],
            0x02,
            "clearing STOP continues from the frozen instant"
        );
    }

    #[test]
    fn fixed_clock_stops_when_virtual_time_stops_and_live_does_not() {
        let cycles = Arc::new(AtomicU64::new(0));
        let mut rtc = Pcf8563::new(50_000, cycles.clone());
        assert_eq!(rtc.policy(), ClockPolicy::Fixed);
        let held = read_time(&mut rtc);
        // Emulator pause: nobody advances the cycle counter.
        assert_eq!(read_time(&mut rtc), held);

        rtc.set_live_unix(1_700_000_000);
        rtc.set_policy(ClockPolicy::Live);
        let live = read_time(&mut rtc);
        rtc.set_live_unix(1_700_000_000 + 86_400);
        let next = read_time(&mut rtc);
        assert_ne!(
            live[3], next[3],
            "live policy follows the host clock while virtual time is paused"
        );
        assert_eq!(cycles.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn alarm_irq_asserts_until_the_flag_is_cleared() {
        let cycles = Arc::new(AtomicU64::new(0));
        let mut rtc = Pcf8563::new(1_000, cycles.clone()); // 00:16:40
                                                           // Minute alarm 17, other fields disabled, AIE on. AF clear.
        rtc.start(false);
        rtc.write(0x09);
        rtc.write(0x17); // minute
        rtc.write(AE); // hour disabled
        rtc.write(AE); // day disabled
        rtc.write(AE); // weekday disabled
        rtc.stop();
        rtc.start(false);
        rtc.write(0x01);
        rtc.write(AIE);
        rtc.stop();
        assert!(!rtc.int_is_low(), "not yet the alarm minute");
        cycles.store(20 * CPU_HZ, Ordering::Relaxed); // 00:17:00
        assert!(rtc.int_is_low(), "AF + AIE pulls INT low");
        cycles.store(80 * CPU_HZ, Ordering::Relaxed); // 00:18:00, flag stays set
        assert!(rtc.int_is_low(), "AF is sticky after the minute passes");
        rtc.start(false);
        rtc.write(0x01);
        rtc.write(AIE); // ClearAlarmFlag keeps AIE and clears AF
        rtc.stop();
        assert!(!rtc.int_is_low(), "clearing AF releases INT");
    }

    #[test]
    fn clearing_af_inside_the_alarm_minute_keeps_int_released() {
        let cycles = Arc::new(AtomicU64::new(0));
        let mut rtc = Pcf8563::new(1_000, cycles.clone()); // 00:16:40
        rtc.start(false);
        for b in [0x09, 0x17, AE, AE, AE] {
            rtc.write(b);
        }
        rtc.stop();
        rtc.start(false);
        rtc.write(0x01);
        rtc.write(AIE);
        rtc.stop();
        cycles.store(21 * CPU_HZ, Ordering::Relaxed); // 00:17:01
        assert!(rtc.int_is_low());
        rtc.start(false);
        rtc.write(0x01);
        rtc.write(AIE);
        rtc.stop();
        cycles.store(50 * CPU_HZ, Ordering::Relaxed); // 00:17:30, same minute
        assert!(!rtc.int_is_low(), "no second match within the minute");
        cycles.store(3620 * CPU_HZ, Ordering::Relaxed); // 01:17:00, next match
        assert!(rtc.int_is_low());
    }

    #[test]
    fn one_hertz_timer_irq_counts_down() {
        let cycles = Arc::new(AtomicU64::new(0));
        let mut rtc = Pcf8563::new(0, cycles.clone());
        rtc.start(false);
        rtc.write(0x0f);
        rtc.write(2);
        rtc.stop();
        rtc.start(false);
        rtc.write(0x0e);
        rtc.write(TIMER_EN | TIMER_1HZ);
        rtc.stop();
        rtc.start(false);
        rtc.write(0x01);
        rtc.write(TIE);
        rtc.stop();
        cycles.store(CPU_HZ, Ordering::Relaxed);
        assert!(!rtc.int_is_low());
        cycles.store(2 * CPU_HZ, Ordering::Relaxed);
        assert!(rtc.int_is_low(), "countdown of 2 s sets TF");
    }

    #[test]
    fn civil_round_trips_leap_days() {
        for t in [
            0i64,
            951_782_400,   /* 2000-02-29 */
            4_107_542_400, /* 2100-03-01 */
        ] {
            let (y, m, d, ..) = civil(t);
            assert_eq!(
                days_from_civil(y, m as i64, d as i64) * 86_400,
                t - t.rem_euclid(86_400)
            );
        }
    }
}
