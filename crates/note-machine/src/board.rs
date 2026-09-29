//! The NOTE baseboard (reference/wiring.md) as an esp32sim `BoardModel`: the SSD2683 panel on
//! the profile's SPI host, the I2C devices, buttons, charger inputs, latch/rails and the NOTE4
//! power LED. Profiles select the pins and the panel variant; nothing here is device-specific.
//!
//! The host (runtime) talks to the board through [`BoardHandle`], a shared view of the state
//! the emulation owner mutates. Host input changes GPIO levels directly on the SoC between run
//! slices and records them here so a guest reset reconnects the same levels.
//!
//! Battery latch GPIO17 high keeps the battery rail closed. Opening it on battery power is a
//! board power-off, not a chip reset: the e-paper image stays, `power_offs` counts the event,
//! and `power_on_pending` asks the machine for a later cold boot (POWER-01). External power
//! keeps the SoC up when the latch opens. `esp_deep_sleep_start` is not modelled in the SoC.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use esp_periph::i2c::I2cDevice;
use esp_soc::board::{BoardEdge, BoardModel};
use note_core::Profile;

use crate::pcf8563::Pcf8563;
use crate::ssd2683::{Ssd2683, Timing};

/// What the charger pins and the battery voltage say, before the guest's own debounce.
/// This is the electrical reading, not a UI percentage.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChargeSense {
    Discharging,
    Charging,
    Full,
    /// Absent battery (0 mV and no charger activity), contradictory charge+full,
    /// or a "full" request the profile's unverified polarity cannot express.
    Unknown,
}

/// Board state shared between the emulation owner and the host. A snapshot (G7) keeps all of
/// it; the RTC chip's registers travel with the I2C bus that owns it (`I2cDevice::save`).
#[derive(serde::Serialize, serde::Deserialize)]
pub struct BoardState {
    pub panel: Ssd2683,
    /// Output levels the firmware drives, by GPIO (only the pins the board cares about).
    #[serde(with = "emu_core::snap::arr")]
    pub outputs: [Option<bool>; 49],
    /// Board-driven input levels that persist across a guest reset (buttons, charger).
    pub inputs: Vec<(u8, bool)>,
    pub spi_bytes: u64,
    /// Battery terminal voltage in mV (Spec §7.3: the hardware input is voltage, not percent).
    pub battery_mv: u32,
    pub adc_conversions: u64,
    pub rtc: Pcf8563,
    /// The audio codec (its registers travel with the I2C bus in a snapshot).
    pub codec: crate::es8311::Es8311,
    led: Option<(u8, bool)>,
    charge_detect: (u8, u8),
    charge_full: (u8, Option<u8>),
    /// High closes the battery rail. Starts closed: something powered the board on.
    pub battery_rail: bool,
    pub external_power: bool,
    pub soc_powered: bool,
    /// Active-low LED is lit. Dark when the SoC has no rail, whatever the last GPIO was.
    pub led_lit: bool,
    pub charge: ChargeSense,
    /// Latch releases on battery power. Not a reset count.
    pub power_offs: u64,
    /// Set when a dead rail comes back (USB inserted, or the power button closes the latch).
    /// The machine turns that into one cold boot. The latch edge itself does not.
    pub power_on_pending: bool,
}

impl BoardState {
    fn set_input(&mut self, gpio: u8, level: bool) {
        match self.inputs.iter_mut().find(|(pin, _)| *pin == gpio) {
            Some(entry) => entry.1 = level,
            None => self.inputs.push((gpio, level)),
        }
    }

    fn input_level(&self, gpio: u8) -> Option<bool> {
        self.inputs
            .iter()
            .find(|(pin, _)| *pin == gpio)
            .map(|(_, level)| *level)
    }

    pub fn set_battery_mv(&mut self, mv: u32) {
        self.battery_mv = mv;
        self.recompute_charge();
    }

    /// `full: Some(true)` on a profile whose full-pin polarity is unverified does not
    /// invent a GPIO level and reports [`ChargeSense::Unknown`].
    pub fn set_charger(&mut self, charging: bool, full: Option<bool>) -> ChargeSense {
        let (detect, charging_level) = self.charge_detect;
        let detect_level = if charging {
            charging_level != 0
        } else {
            charging_level == 0
        };
        self.set_input(detect, detect_level);
        if let (Some(want), Some(full_level)) = (full, self.charge_full.1) {
            let level = if want {
                full_level != 0
            } else {
                full_level == 0
            };
            self.set_input(self.charge_full.0, level);
        }
        self.recompute_charge();
        if full == Some(true) && self.charge_full.1.is_none() {
            self.charge = ChargeSense::Unknown;
        }
        self.charge
    }

    fn recompute_charge(&mut self) {
        let (detect, charging_level) = self.charge_detect;
        let charging = self
            .input_level(detect)
            .is_some_and(|level| level == (charging_level != 0));
        let full = match self.charge_full.1 {
            Some(full_level) => self
                .input_level(self.charge_full.0)
                .is_some_and(|level| level == (full_level != 0)),
            None => false,
        };
        self.charge = if self.battery_mv == 0 && !charging && !full {
            ChargeSense::Unknown
        } else if charging && full {
            ChargeSense::Unknown
        } else if full {
            ChargeSense::Full
        } else if charging {
            ChargeSense::Charging
        } else {
            ChargeSense::Discharging
        };
    }

    pub fn set_external_power(&mut self, on: bool) {
        self.external_power = on;
        self.recompute_rail();
    }

    /// Power button closed the latch again. Not itself a reset.
    pub fn close_battery_latch(&mut self) {
        self.battery_rail = true;
        self.recompute_rail();
    }

    /// The profile has the status LED (both NOTE4 and NOTE4C, GPIO3).
    pub fn has_led(&self) -> bool {
        self.led.is_some()
    }

    fn recompute_led(&mut self) {
        self.led_lit = match self.led {
            Some((gpio, active_low)) if self.soc_powered => self.outputs[gpio as usize]
                .is_some_and(|level| if active_low { !level } else { level }),
            _ => false,
        };
    }

    fn recompute_rail(&mut self) {
        let on = self.battery_rail || self.external_power;
        if on == self.soc_powered {
            self.recompute_led();
            return;
        }
        self.soc_powered = on;
        if on {
            self.power_on_pending = true;
            self.recompute_led();
        } else {
            self.power_offs += 1;
            self.led_lit = false;
            // The e-paper glass keeps its image; the controller loses its rail.
            self.panel.set_power(false);
        }
    }

    fn note_latch(&mut self, level: bool) {
        self.battery_rail = level;
        self.recompute_rail();
    }

    fn note_led(&mut self) {
        self.recompute_led();
    }
}

#[derive(Clone)]
pub struct BoardHandle(Arc<Mutex<BoardState>>);

impl BoardHandle {
    pub fn lock(&self) -> MutexGuard<'_, BoardState> {
        self.0.lock().expect("board state poisoned")
    }
}

struct Pins {
    battery_adc: (u8, u8),
    battery_divider: f64,
    spi_host: u8,
    power: u8,
    busy: u8,
    reset: u8,
    dc: u8,
    cs: u8,
    latch: u8,
    led: Option<u8>,
}

pub struct NoteBoard {
    name: &'static str,
    pins: Pins,
    watched: Vec<u8>,
    state: BoardHandle,
    cycles: Arc<AtomicU64>,
    rtc_int: Option<u8>,
    rtc_level: bool,
    busy_level: bool,
    edges: Vec<BoardEdge>,
}

impl NoteBoard {
    pub fn new(profile: &Profile, rtc_base_unix: i64) -> (NoteBoard, BoardHandle) {
        let d = &profile.display;
        let adc = &profile.power.battery_adc;
        let led = profile.leds.first().map(|l| (l.gpio, l.active_low));
        let pins = Pins {
            battery_adc: (adc.unit, adc.channel),
            battery_divider: adc.divider,
            spi_host: d.spi_host,
            power: d.pins.power,
            busy: d.pins.busy,
            reset: d.pins.reset,
            dc: d.pins.dc,
            cs: d.pins.cs,
            latch: profile.power.battery_latch_gpio,
            led: led.map(|(gpio, _)| gpio),
        };
        let mut watched = vec![
            pins.power,
            pins.reset,
            pins.dc,
            pins.cs,
            pins.latch,
            profile.audio.rail_gpio,
            profile.audio.amp_gpio,
        ];
        watched.extend(profile.leds.iter().map(|l| l.gpio));
        // Buttons idle released; DOWN (GPIO18) must read high for the factory Power_Init (ADR-009).
        let mut inputs: Vec<(u8, bool)> = profile
            .buttons
            .iter()
            .map(|b| (b.gpio, b.active_low))
            .collect();
        // Charger idle: not charging. Full pin only when this profile knows the polarity.
        inputs.push((
            profile.power.charge_detect.gpio,
            profile.power.charge_detect.charging_level == 0,
        ));
        if let Some(full) = profile.power.charge_full.full_level {
            inputs.push((profile.power.charge_full.gpio, full == 0));
        }
        let cycles = Arc::new(AtomicU64::new(0));
        let rtc = Pcf8563::new(rtc_base_unix, cycles.clone());
        let rtc_int = profile
            .i2c
            .devices
            .iter()
            .find(|d| d.kind == "pcf8563")
            .and_then(|d| d.int_gpio);
        let mut state_inner = BoardState {
            panel: Ssd2683::new(d.variant, Timing::fast()),
            outputs: [None; 49],
            inputs,
            spi_bytes: 0,
            battery_mv: 3900,
            adc_conversions: 0,
            rtc,
            codec: crate::es8311::Es8311::new(),
            led,
            charge_detect: (
                profile.power.charge_detect.gpio,
                profile.power.charge_detect.charging_level,
            ),
            charge_full: (
                profile.power.charge_full.gpio,
                profile.power.charge_full.full_level,
            ),
            battery_rail: true,
            external_power: false,
            soc_powered: true,
            led_lit: false,
            charge: ChargeSense::Discharging,
            power_offs: 0,
            power_on_pending: false,
        };
        state_inner.recompute_charge();
        let state = BoardHandle(Arc::new(Mutex::new(state_inner)));
        let name = if profile.id == "note4" {
            "note4"
        } else {
            "note4c"
        };
        let board = NoteBoard {
            name,
            pins,
            watched,
            state: state.clone(),
            cycles,
            rtc_int,
            rtc_level: true,
            busy_level: true,
            edges: Vec::new(),
        };
        (board, state)
    }

    fn apply_pin(&self, st: &mut BoardState, pin: u8, level: bool) {
        let p = &self.pins;
        if pin == p.power {
            st.panel.set_power(level);
        } else if pin == p.reset {
            st.panel.set_reset(level);
        } else if pin == p.dc {
            st.panel.set_dc(level);
        } else if pin == p.cs {
            st.panel.set_cs(!level);
        } else if pin == p.latch {
            st.note_latch(level);
        } else if p.led == Some(pin) {
            st.note_led();
        }
    }

    fn sample_busy(&mut self, st: &BoardState) {
        let level = st.panel.busy_level();
        if level != self.busy_level {
            self.busy_level = level;
            self.edges.push(BoardEdge {
                cycle: self.cycles.load(Ordering::Relaxed),
                pin: self.pins.busy,
                level,
            });
        }
    }

    fn sample_rtc(&mut self, st: &BoardState) {
        let Some(pin) = self.rtc_int else { return };
        let level = !st.rtc.int_is_low();
        if level != self.rtc_level {
            self.rtc_level = level;
            self.edges.push(BoardEdge {
                cycle: self.cycles.load(Ordering::Relaxed),
                pin,
                level,
            });
        }
    }
}

impl BoardModel for NoteBoard {
    fn name(&self) -> &'static str {
        self.name
    }

    fn gpio_changes(&mut self, changes: &[(u8, bool)]) {
        let handle = self.state.clone();
        let mut st = handle.lock();
        for &(pin, level) in changes {
            if (pin as usize) < 49 && self.watched.contains(&pin) {
                st.outputs[pin as usize] = Some(level);
                self.apply_pin(&mut st, pin, level);
            }
        }
        self.sample_busy(&st);
    }

    fn spi_transfer(&mut self, host: u8, tx: &[u8], rx_len: usize) -> Vec<u8> {
        if host != self.pins.spi_host {
            return vec![0xff; rx_len];
        }
        let handle = self.state.clone();
        let mut st = handle.lock();
        st.spi_bytes += (tx.len() + rx_len) as u64;
        let rx = st.panel.spi(tx, rx_len);
        self.sample_busy(&st);
        rx
    }

    fn i2c_devices(&mut self) -> Vec<(u8, u8, Box<dyn I2cDevice>)> {
        let rtc = self.state.lock().rtc.clone();
        let codec = self.state.lock().codec.clone();
        vec![
            (0, 0x51, Box::new(rtc)),
            // ES8311: one chip for the life of the board (a SoC reset does not reset it).
            (0, 0x18, Box::new(codec)),
            // NFC (0x55) is absent: the controller NACKs unknown addresses.
        ]
    }

    fn adc_millivolts(&mut self, unit: u8, channel: u8) -> Option<u32> {
        if (unit, channel) != self.pins.battery_adc {
            return None;
        }
        let mut st = self.state.lock();
        st.adc_conversions += 1;
        Some((st.battery_mv as f64 / self.pins.battery_divider).round() as u32)
    }

    fn input_levels(&self) -> Vec<(u8, bool)> {
        let st = self.state.lock();
        let mut levels = st.inputs.clone();
        levels.push((self.pins.busy, st.panel.busy_level()));
        if let Some(pin) = self.rtc_int {
            levels.push((pin, !st.rtc.int_is_low()));
        }
        levels
    }

    fn next_deadline(&self) -> Option<u64> {
        let st = self.state.lock();
        match (st.panel.next_deadline(), st.rtc.next_deadline()) {
            (Some(panel), Some(rtc)) => Some(panel.min(rtc)),
            (panel, rtc) => panel.or(rtc),
        }
    }

    fn advance_to(&mut self, cycle: u64) {
        self.cycles.store(cycle, Ordering::Relaxed);
        let handle = self.state.clone();
        let mut st = handle.lock();
        st.panel.advance_to(cycle);
        self.sample_busy(&st);
        self.sample_rtc(&st);
    }

    fn take_edges(&mut self) -> Vec<BoardEdge> {
        std::mem::take(&mut self.edges)
    }

    fn named_pin(&self, name: &str) -> Option<u8> {
        match name {
            "busy" => Some(self.pins.busy),
            _ => None,
        }
    }

    fn report(&self) -> String {
        let st = self.state.lock();
        format!(
            "[note] panel: {} refreshes, {} SPI bytes, {} diagnostics; {} battery ADC conversions; rail {}",
            st.panel.refreshes.len(),
            st.spi_bytes,
            st.panel.diagnostics.len(),
            st.adc_conversions,
            if st.soc_powered { "on" } else { "off" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcf8563::ClockPolicy;
    use esp_periph::i2c::I2cDevice;
    use note_core::profile::{self, profiles_dir};

    fn board(id: &str) -> (NoteBoard, BoardHandle) {
        let profile = profile::find(&profiles_dir(), id).unwrap();
        NoteBoard::new(&profile, 1_000)
    }

    #[test]
    fn latch_release_is_power_off_not_a_reset_and_external_power_is_not() {
        let (mut note4, h) = board("note4");
        let image = h.lock().panel.visible().to_vec();
        note4.gpio_changes(&[(3, false)]); // active-low LED on
        assert!(h.lock().led_lit);
        note4.gpio_changes(&[(17, false)]);
        {
            let st = h.lock();
            assert!(!st.soc_powered);
            assert_eq!(st.power_offs, 1);
            assert!(!st.power_on_pending, "opening the latch is not a boot");
            assert!(!st.led_lit, "no rail, LED is dark");
            assert!(st.panel.busy_level(), "unpowered BUSY is pulled up");
            assert_eq!(
                st.panel.visible(),
                image.as_slice(),
                "e-paper retains the image"
            );
        }
        // Traffic while the rail is down is the panel's unpowered path.
        note4.gpio_changes(&[(6, false)]);
        h.lock().close_battery_latch();
        {
            let st = h.lock();
            assert!(st.soc_powered);
            assert!(st.power_on_pending, "wake is a separate cold boot");
            assert_eq!(st.power_offs, 1);
            assert_eq!(st.panel.visible(), image.as_slice());
        }

        let (mut kept, hk) = board("note4");
        hk.lock().set_external_power(true);
        assert!(!hk.lock().power_on_pending, "already running");
        kept.gpio_changes(&[(6, true), (17, false)]);
        {
            let st = hk.lock();
            assert!(st.soc_powered, "USB keeps the SoC up");
            assert_eq!(st.power_offs, 0);
            assert!(!st.battery_rail);
            assert_eq!(st.panel.visible().len(), image.len());
        }
    }

    #[test]
    fn status_led_tracks_gpio3_active_low_on_both_boards() {
        for id in ["note4", "note4c"] {
            let (mut b, h) = board(id);
            assert!(!h.lock().led_lit, "{id}");
            b.gpio_changes(&[(3, true)]);
            assert!(!h.lock().led_lit, "{id}: high is off");
            b.gpio_changes(&[(3, false)]);
            assert!(h.lock().led_lit, "{id}: low is lit");
        }
    }

    #[test]
    fn charger_pins_and_adc_are_the_guest_measurement() {
        let (mut note4c, h) = board("note4c");
        assert_eq!(h.lock().charge, ChargeSense::Discharging);
        assert_eq!(
            note4c.adc_millivolts(1, 3),
            Some(1950),
            "3900 mV through the 1:2 divider"
        );
        h.lock().set_battery_mv(3700);
        assert_eq!(note4c.adc_millivolts(1, 3), Some(1850));
        assert_eq!(
            h.lock().set_charger(true, Some(false)),
            ChargeSense::Charging
        );
        assert_eq!(h.lock().input_level(2), Some(false), "GPIO2 low = charging");
        assert_eq!(h.lock().set_charger(false, Some(true)), ChargeSense::Full);
        assert_eq!(
            h.lock().input_level(1),
            Some(true),
            "NOTE4C full level is high"
        );
        assert_eq!(
            h.lock().set_charger(true, Some(true)),
            ChargeSense::Unknown,
            "charge and full together"
        );
        h.lock().set_battery_mv(0);
        assert_eq!(
            h.lock().set_charger(false, Some(false)),
            ChargeSense::Unknown,
            "0 mV, no charger"
        );
        // The reading stays the voltage. Nothing here stores a percent.
        assert_eq!(h.lock().battery_mv, 0);

        let (_note4, n4) = board("note4");
        assert_eq!(
            n4.lock().set_charger(false, Some(true)),
            ChargeSense::Unknown,
            "NOTE4 full polarity is unverified"
        );
        assert!(n4.lock().input_level(1).is_none(), "do not invent GPIO1");
        assert_eq!(n4.lock().set_charger(true, None), ChargeSense::Charging);
    }

    #[test]
    fn rtc_alarm_edge_reaches_gpio5_and_survives_reattach() {
        let (mut board, h) = board("note4");
        assert!(board.input_levels().contains(&(5, true)), "INT idles high");
        {
            let rtc = h.lock().rtc.clone();
            let mut rtc = rtc;
            rtc.start(false);
            rtc.write(0x09);
            rtc.write(0x17);
            rtc.write(0x80);
            rtc.write(0x80);
            rtc.write(0x80);
            rtc.stop();
            rtc.start(false);
            rtc.write(0x01);
            rtc.write(1 << 1); // AIE
            rtc.stop();
        }
        board.advance_to(20 * 240_000_000);
        let edges = board.take_edges();
        assert!(
            edges.iter().any(|e| e.pin == 5 && !e.level),
            "alarm pulls GPIO5 low: {edges:?}"
        );
        assert!(board.input_levels().contains(&(5, false)));

        // Guest reset builds a new I2C wrapper around the same chip.
        let mut first = board.i2c_devices();
        let dev = &mut first
            .iter_mut()
            .find(|(_, addr, _)| *addr == 0x51)
            .unwrap()
            .2;
        dev.start(false);
        dev.write(0x02);
        for b in [0x00, 0x00, 0x12, 0x02, 0x03, 0x04, 0x26] {
            dev.write(b); // 2026-04-02 12:00:00
        }
        dev.stop();
        drop(first);
        let mut second = board.i2c_devices();
        let dev = &mut second
            .iter_mut()
            .find(|(_, addr, _)| *addr == 0x51)
            .unwrap()
            .2;
        dev.start(false);
        dev.write(0x04); // hours
        dev.stop();
        dev.start(true);
        assert_eq!(dev.read(), 0x12, "guest-set time survives reattach");
        assert_eq!(h.lock().rtc.policy(), ClockPolicy::Fixed);
    }

    #[test]
    fn note4_and_note4c_keep_independent_panels_controls_and_clocks() {
        let (mut a, ha) = board("note4");
        let (mut b, hb) = board("note4c");
        ha.lock().set_battery_mv(3500);
        hb.lock().set_battery_mv(4100);
        a.gpio_changes(&[(17, true)]);
        b.gpio_changes(&[(17, false)]);
        {
            let mut sta = ha.lock();
            sta.panel.set_power(true);
            sta.panel.set_reset(true);
        }
        {
            let stb = hb.lock();
            assert_eq!(stb.battery_mv, 4100);
            assert!(!stb.soc_powered, "NOTE4C latch is its own");
            assert_ne!(
                stb.panel.visible().len(),
                ha.lock().panel.visible().len(),
                "gray4 vs pal2 frames"
            );
        }
        assert!(ha.lock().soc_powered);
        assert_eq!(ha.lock().battery_mv, 3500);
        // Clocks are different chips.
        ha.lock().rtc.set_live_unix(10);
        hb.lock().rtc.set_live_unix(99);
        ha.lock().rtc.set_policy(ClockPolicy::Live);
        assert_eq!(hb.lock().rtc.policy(), ClockPolicy::Fixed);
    }
}
