//! Solomon SSD2683 e-paper controller as the NOTE boards use it (ADR-013).
//!
//! Both NOTE variants drive the same controller over 4-wire SPI (DC/CS/RST/BUSY as GPIOs,
//! 3-wire readback on MOSI). The RAM holds a 2-bit code per pixel, 400 × 300 = 30 000 bytes,
//! row-major, first pixel in the top bits. What a code *looks like* depends on the panel:
//! the NOTE4C four-colour panel shows 00 black · 01 white · 10 yellow · 11 red; the NOTE4
//! monochrome panel shows 00 black · 01 white, and uses transition codes (old<<1 | new) for
//! partial refresh.
//!
//! Written from the drivers that exercise it (clean-room, ADR-006): the MIT NOTE4 reference
//! `zectrix_epd.cc`, the NOTE4C xiaozhi `custom_lcd_display.cc`, emini `home_panel.c`.
//!
//! Timing: BUSY is active low. Every operation that the drivers wait on drives BUSY low for a
//! virtual-time duration from the selected preset, so "BUSY asserted, then released" is
//! observable (emini checks the assertion explicitly).

use note_core::profile::PanelVariant;

pub const WIDTH: usize = 400;
pub const HEIGHT: usize = 300;
pub const RAM_BYTES: usize = WIDTH * HEIGHT / 4;
const RAM_STRIDE: usize = WIDTH / 4;

/// Virtual durations in CPU cycles (240 MHz).
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug)]
pub struct Timing {
    pub reset: u64,
    pub command: u64,
    pub power: u64,
    pub full_refresh: u64,
    pub partial_refresh: u64,
}

impl Timing {
    const MS: u64 = 240_000;
    /// Short but visible waits: firmware sees every BUSY assertion, boots stay quick.
    pub fn fast() -> Timing {
        Timing {
            reset: 2 * Self::MS,
            command: Self::MS,
            power: 5 * Self::MS,
            full_refresh: 150 * Self::MS,
            partial_refresh: 60 * Self::MS,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshKind {
    Full,
    Partial,
    /// One pass of an external-waveform (grayscale) sequence.
    Gray,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Diagnostic {
    /// SPI traffic while the panel rail (GPIO6) was off.
    UnpoweredTraffic { bytes: usize },
    /// Data bytes before any command.
    DataWithoutCommand { bytes: usize },
    /// An external waveform (`0x20`) that is not one of the five known 16-gray passes.
    /// The visible image is kept.
    UnknownPanelWaveform { lut_len: usize },
    /// RAM write ran past the frame or window.
    RamOverflow { command: u8 },
    /// A command byte this model does not know; accepted and ignored.
    UnknownCommand { command: u8 },
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct Refresh {
    pub kind: RefreshKind,
    /// Dirty rectangle in panel pixels: x, y, w, h.
    pub rect: (u16, u16, u16, u16),
    pub cycle: u64,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug)]
struct Window {
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
}

/// Where the BUSY pin sits in virtual time. Not a waveform image.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusyAnchor {
    pub now: u64,
    pub busy_until: u64,
    pub powered: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Ssd2683 {
    variant: PanelVariant,
    timing: Timing,
    now: u64,
    // Pin state as seen from the controller.
    powered: bool,
    in_reset: bool,
    dc_data: bool,
    cs_low: bool,
    // Controller state.
    command: Option<u8>,
    args: Vec<u8>,
    pub panel_setting: [u8; 2],
    vcom_data: u8,
    window: Option<Window>,
    #[serde(with = "emu_core::snap::bytes")]
    ram: Vec<u8>,
    ram_pos: usize,
    external_lut: Option<Vec<u8>>,
    lut_fill: Option<Vec<u8>>,
    asleep: bool,
    pub temperature: u8,
    /// Temperature reads served over the 3-wire readback (SPI-02 evidence).
    pub readbacks: u64,
    busy_until: u64,
    // Output.
    /// Last committed image: panel codes (pal2, NOTE4C) or gray levels (gray4, NOTE4).
    #[serde(with = "emu_core::snap::bytes")]
    visible: Vec<u8>,
    version: u64,
    pub refreshes: Vec<Refresh>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Ssd2683 {
    pub fn new(variant: PanelVariant, timing: Timing) -> Ssd2683 {
        let visible = match variant {
            // Unrefreshed four-colour panel: white.
            PanelVariant::Bwry => vec![0x55; RAM_BYTES],
            // gray4 white = level 15.
            PanelVariant::Mono => vec![0xff; WIDTH * HEIGHT / 2],
        };
        Ssd2683 {
            variant,
            timing,
            now: 0,
            powered: false,
            in_reset: false,
            dc_data: false, // GPIO outputs come out of reset low; setting low again is no edge
            cs_low: false,
            command: None,
            args: Vec::new(),
            panel_setting: [0; 2],
            vcom_data: 0,
            window: None,
            ram: vec![0x55; RAM_BYTES],
            ram_pos: 0,
            external_lut: None,
            lut_fill: None,
            asleep: false,
            temperature: 25,
            readbacks: 0,
            busy_until: 0,
            visible,
            version: 0,
            refreshes: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    pub fn advance_to(&mut self, cycle: u64) {
        self.now = self.now.max(cycle);
    }

    /// BUSY pin level (active low: `false` while an operation runs). An unpowered controller
    /// leaves the pulled-up line high.
    pub fn busy_level(&self) -> bool {
        !self.powered || self.now >= self.busy_until
    }

    /// Cycle at which BUSY will next change, if an operation is running.
    pub fn next_deadline(&self) -> Option<u64> {
        (self.powered && self.busy_until > self.now).then_some(self.busy_until)
    }

    pub fn visible(&self) -> &[u8] {
        &self.visible
    }

    /// BUSY-line anchor for a snapshot. Waveform RAM is not part of this (G5).
    pub fn busy_anchor(&self) -> BusyAnchor {
        BusyAnchor { now: self.now, busy_until: self.busy_until, powered: self.powered }
    }

    pub fn restore_busy_anchor(&mut self, anchor: BusyAnchor) {
        self.now = anchor.now;
        self.busy_until = anchor.busy_until;
        self.powered = anchor.powered;
    }

    /// Replace the canonical visible image. Length must match the variant.
    pub fn restore_visible(&mut self, bytes: &[u8]) -> Result<(), ()> {
        if bytes.len() != self.visible.len() {
            return Err(());
        }
        self.visible.copy_from_slice(bytes);
        Ok(())
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn set_power(&mut self, on: bool) {
        if on == self.powered {
            return;
        }
        self.powered = on;
        if !on {
            // Controller state is lost; the e-paper image stays (retention, Spec §7.2).
            self.reset_controller();
            self.busy_until = 0;
        }
    }

    pub fn set_reset(&mut self, high: bool) {
        let entering = !high;
        if entering == self.in_reset {
            return;
        }
        self.in_reset = entering;
        if !entering && self.powered {
            // Leaving reset: registers to defaults, BUSY low while the controller boots.
            self.reset_controller();
            self.busy_for(self.timing.reset);
        }
    }

    pub fn set_dc(&mut self, data: bool) {
        self.dc_data = data;
    }

    pub fn set_cs(&mut self, low: bool) {
        self.cs_low = low;
    }

    fn reset_controller(&mut self) {
        self.finish_command();
        self.command = None;
        self.panel_setting = [0; 2];
        self.vcom_data = 0;
        self.window = None;
        self.external_lut = None;
        self.asleep = false;
    }

    fn busy_for(&mut self, cycles: u64) {
        self.busy_until = self.now + cycles;
    }

    /// One SPI transaction on the panel host. `tx` is what the master shifted out; the return
    /// value is what the controller drives back for `rx_len` bytes (temperature readback).
    pub fn spi(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        if !self.powered || self.in_reset {
            if !tx.is_empty() {
                self.diagnostics
                    .push(Diagnostic::UnpoweredTraffic { bytes: tx.len() });
            }
            return vec![0xff; rx_len];
        }
        if rx_len > 0 {
            // The only register the NOTE drivers read: temperature after `0x40`.
            let value = if self.command == Some(0x40) {
                self.readbacks += 1;
                self.temperature
            } else {
                0xff
            };
            return vec![value; rx_len];
        }
        if self.dc_data {
            self.data(tx);
        } else {
            for &c in tx {
                self.begin_command(c);
            }
        }
        Vec::new()
    }

    fn begin_command(&mut self, c: u8) {
        self.finish_command();
        self.command = Some(c);
        self.args.clear();
        if self.asleep {
            return; // only a hardware reset wakes the controller
        }
        match c {
            0x04 => self.busy_for(self.timing.power), // internal power on
            0x10 => {
                self.ram_pos = 0;
                self.busy_for(self.timing.command);
            }
            0x20 => self.lut_fill = Some(Vec::new()),
            0x40 => self.busy_for(self.timing.command), // temperature measurement
            0xA5 => self.busy_for(self.timing.command), // temperature/OTP activation
            0x00 | 0x01 | 0x02 | 0x06 | 0x07 | 0x12 | 0x30 | 0x50 | 0x61 | 0x62 | 0x65 | 0x82
            | 0x83 | 0xE0 | 0xE6 | 0xE7 | 0xE9 | 0x44 | 0x45 | 0x4E | 0x4F => {}
            _ => self
                .diagnostics
                .push(Diagnostic::UnknownCommand { command: c }),
        }
    }

    /// Commands whose parameters were complete when the next command (or a reset) arrives.
    fn finish_command(&mut self) {
        if let (Some(0x20), Some(lut)) = (self.command, self.lut_fill.take()) {
            self.external_lut = Some(lut);
        }
    }

    fn data(&mut self, bytes: &[u8]) {
        let Some(command) = self.command else {
            self.diagnostics
                .push(Diagnostic::DataWithoutCommand { bytes: bytes.len() });
            return;
        };
        if self.asleep {
            return;
        }
        match command {
            0x10 => self.write_ram(bytes),
            0x20 => {
                if let Some(lut) = &mut self.lut_fill {
                    lut.extend_from_slice(bytes);
                }
            }
            _ => {
                for &b in bytes {
                    self.args.push(b);
                    self.argument(command, self.args.len(), b);
                }
            }
        }
    }

    /// React to argument number `n` (1-based) of `command`.
    fn argument(&mut self, command: u8, n: usize, value: u8) {
        match (command, n) {
            (0x00, 1 | 2) => {
                self.panel_setting[n - 1] = value;
                if n == 2 {
                    // A new panel setting selects OTP waveforms again.
                    self.external_lut = None;
                }
            }
            (0xE9, 1) => self.busy_for(self.timing.command), // OTP / cascade select
            (0x50, 1) => self.vcom_data = value,
            (0x02, 1) => self.busy_for(self.timing.power), // internal power off
            (0x07, 1) if value == 0xA5 => self.asleep = true, // deep sleep
            (0x12, 1) => self.refresh(),
            (0x83, 9) => {
                let a = &self.args;
                let word = |i: usize| ((a[i] as usize & 0x03) << 8) | a[i + 1] as usize;
                let (x0, x1, y0, y1) = (word(0), word(2), word(4), word(6));
                self.window = (value & 1 != 0 && x0 <= x1 && y0 <= y1 && x1 < WIDTH && y1 < HEIGHT)
                    .then_some(Window { x0, x1, y0, y1 });
                self.ram_pos = 0;
            }
            _ => {}
        }
    }

    fn write_ram(&mut self, bytes: &[u8]) {
        let (stride, rows) = match self.window {
            Some(w) => ((w.x1 - w.x0 + 1) / 4, w.y1 - w.y0 + 1),
            None => (RAM_STRIDE, HEIGHT),
        };
        for &b in bytes {
            if self.ram_pos >= stride * rows {
                self.diagnostics
                    .push(Diagnostic::RamOverflow { command: 0x10 });
                return;
            }
            let (row, col) = (self.ram_pos / stride, self.ram_pos % stride);
            let index = match self.window {
                Some(w) => (w.y0 + row) * RAM_STRIDE + w.x0 / 4 + col,
                None => row * RAM_STRIDE + col,
            };
            self.ram[index] = b;
            self.ram_pos += 1;
        }
    }

    fn code(&self, x: usize, y: usize) -> u8 {
        (self.ram[y * RAM_STRIDE + x / 4] >> (6 - 2 * (x % 4))) & 3
    }

    fn set_gray(&mut self, x: usize, y: usize, level: u8) {
        let i = y * WIDTH + x;
        let shift = if i % 2 == 0 { 4 } else { 0 };
        self.visible[i / 2] = (self.visible[i / 2] & !(0x0f << shift)) | (level << shift);
    }

    fn refresh(&mut self) {
        let transition = self.vcom_data == 0x77;
        if let Some(lut) = &self.external_lut {
            let pass = (self.variant == PanelVariant::Mono)
                .then(|| crate::gray16::recognize(lut))
                .flatten();
            if let Some(pass) = pass {
                for y in 0..HEIGHT {
                    for x in 0..WIDTH {
                        if let Some(level) = crate::gray16::level_of(pass, self.code(x, y)) {
                            self.set_gray(x, y, level);
                        }
                    }
                }
                self.busy_for(self.timing.full_refresh);
                self.version += 1;
                self.refreshes.push(Refresh {
                    kind: RefreshKind::Gray,
                    rect: (0, 0, WIDTH as u16, HEIGHT as u16),
                    cycle: self.now,
                });
                return;
            }
            // Not a decoded pass, or the four-colour panel. Do not guess pixels.
            let lut_len = lut.len();
            self.diagnostics
                .push(Diagnostic::UnknownPanelWaveform { lut_len });
            self.busy_for(self.timing.full_refresh);
            return;
        }
        let (kind, rect) = if transition {
            let w = self.window.unwrap_or(Window {
                x0: 0,
                x1: WIDTH - 1,
                y0: 0,
                y1: HEIGHT - 1,
            });
            for y in w.y0..=w.y1 {
                for x in w.x0..=w.x1 {
                    let new_white = self.code(x, y) & 1 != 0;
                    self.commit_pixel(x, y, if new_white { 1 } else { 0 });
                }
            }
            (
                RefreshKind::Partial,
                (
                    w.x0 as u16,
                    w.y0 as u16,
                    (w.x1 - w.x0 + 1) as u16,
                    (w.y1 - w.y0 + 1) as u16,
                ),
            )
        } else {
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let code = self.code(x, y);
                    self.commit_pixel(x, y, code);
                }
            }
            (RefreshKind::Full, (0, 0, WIDTH as u16, HEIGHT as u16))
        };
        self.busy_for(match kind {
            RefreshKind::Partial => self.timing.partial_refresh,
            _ => self.timing.full_refresh,
        });
        self.version += 1;
        self.refreshes.push(Refresh {
            kind,
            rect,
            cycle: self.now,
        });
    }

    /// Show panel `code` at (x, y) in the variant's canonical format.
    fn commit_pixel(&mut self, x: usize, y: usize, code: u8) {
        match self.variant {
            PanelVariant::Bwry => {
                let i = y * RAM_STRIDE + x / 4;
                let shift = 6 - 2 * (x % 4);
                self.visible[i] = (self.visible[i] & !(3 << shift)) | (code << shift);
            }
            // Monochrome panel: bit 0 of an OTP code is the ink (01 white, 00 black).
            PanelVariant::Mono => self.set_gray(x, y, if code & 1 != 0 { 15 } else { 0 }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 240_000;

    /// Drive the controller the way the drivers do: DC/CS around each byte group.
    struct Driver<'a>(&'a mut Ssd2683);
    impl Driver<'_> {
        fn cmd(&mut self, c: u8) {
            self.0.set_dc(false);
            self.0.spi(&[c], 0);
        }
        fn data(&mut self, d: &[u8]) {
            self.0.set_dc(true);
            self.0.spi(d, 0);
        }
        fn read(&mut self) -> u8 {
            self.0.set_dc(true);
            self.0.spi(&[], 1)[0]
        }
        fn wait_busy(&mut self) {
            assert!(!self.0.busy_level(), "operation must assert BUSY");
            let until = self.0.next_deadline().expect("deadline");
            self.0.advance_to(until);
            assert!(self.0.busy_level(), "BUSY released at its deadline");
        }
    }

    fn powered(variant: PanelVariant) -> Ssd2683 {
        let mut p = Ssd2683::new(variant, Timing::fast());
        p.set_power(true);
        p.set_reset(false);
        p.set_reset(true);
        p
    }

    /// NOTE4 reference `WriteOtpFrame` + `TriggerOtpRefresh` for a 1bpp image.
    fn note4_full_1bpp(p: &mut Ssd2683, image: &[u8]) {
        let mut d = Driver(p);
        d.wait_busy(); // reset
        d.cmd(0x00);
        d.data(&[0x2F, 0x0E]);
        d.cmd(0xE9);
        d.data(&[0x01]);
        d.wait_busy();
        d.cmd(0x40);
        d.wait_busy();
        assert_eq!(d.read(), 25, "temperature readback");
        d.cmd(0xE0);
        d.data(&[0x02]);
        d.cmd(0xE6);
        d.data(&[241]);
        d.cmd(0xA5);
        d.wait_busy();
        d.cmd(0x10);
        for y in 0..HEIGHT {
            let mut line = [0u8; RAM_STRIDE];
            for xb in 0..WIDTH / 8 {
                let input = image[y * 50 + xb];
                let (mut first, mut second) = (0u8, 0u8);
                for bit in 0..8 {
                    let pixel = (input >> (7 - bit)) & 1;
                    if bit < 4 {
                        first |= pixel << (6 - bit * 2)
                    } else {
                        second |= pixel << (14 - bit * 2)
                    }
                }
                line[xb * 2] = first;
                line[xb * 2 + 1] = second;
            }
            d.data(&line);
        }
        d.cmd(0x04);
        d.wait_busy();
        d.cmd(0x12);
        d.data(&[0x00]);
        d.wait_busy();
        d.cmd(0x02);
        d.data(&[0x00]);
        d.wait_busy();
    }

    fn gray_at(p: &Ssd2683, x: usize, y: usize) -> u8 {
        let i = y * WIDTH + x;
        (p.visible()[i / 2] >> if i % 2 == 0 { 4 } else { 0 }) & 0x0f
    }

    #[test]
    fn note4_full_monochrome_frame_matches_the_1bpp_source() {
        let mut p = powered(PanelVariant::Mono);
        // Left half black, right half white, plus one white pixel at (3, 7) in the black half.
        let mut image = vec![0u8; 50 * HEIGHT];
        for y in 0..HEIGHT {
            for xb in 25..50 {
                image[y * 50 + xb] = 0xff;
            }
        }
        image[7 * 50] |= 0x80 >> 3;
        note4_full_1bpp(&mut p, &image);
        assert_eq!(p.version(), 1);
        assert_eq!(p.refreshes[0].kind, RefreshKind::Full);
        assert_eq!(gray_at(&p, 0, 0), 0);
        assert_eq!(gray_at(&p, 3, 7), 15);
        assert_eq!(gray_at(&p, 199, 150), 0);
        assert_eq!(gray_at(&p, 200, 150), 15);
        assert_eq!(gray_at(&p, 399, 299), 15);
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    }

    #[test]
    fn note4_partial_window_changes_only_the_window() {
        let mut p = powered(PanelVariant::Mono);
        note4_full_1bpp(&mut p, &vec![0xffu8; 50 * HEIGHT]); // all white
        let mut d = Driver(&mut p);
        // zectrix_epd_refresh_partial_1bpp: 64x32 black patch at (80, 96).
        d.cmd(0x50);
        d.data(&[0x77]);
        d.cmd(0x83);
        d.data(&[0, 80, 0, 143, 0, 96, 0, 127, 0x01]);
        d.cmd(0x10);
        let stride = (64 / 8) * 2;
        for _ in 0..32 {
            // old white (1) -> new black (0): transition code 0b10 for every pixel
            d.data(&vec![0xAA; stride]);
        }
        d.cmd(0x04);
        d.wait_busy();
        d.cmd(0x12);
        d.data(&[0x00]);
        let r = p.refreshes.last().unwrap().clone();
        assert_eq!((r.kind, r.rect), (RefreshKind::Partial, (80, 96, 64, 32)));
        assert_eq!(gray_at(&p, 80, 96), 0);
        assert_eq!(gray_at(&p, 143, 127), 0);
        assert_eq!(gray_at(&p, 79, 96), 15, "left of the window unchanged");
        assert_eq!(gray_at(&p, 144, 127), 15, "right of the window unchanged");
        assert_eq!(gray_at(&p, 80, 128), 15, "below the window unchanged");
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    }

    #[test]
    fn note4c_four_colour_codes_are_kept_as_palette_indices() {
        // emini home_panel.c path: no 0x00 panel setting, OTP select, raw 2bpp frame.
        let mut p = powered(PanelVariant::Bwry);
        let mut d = Driver(&mut p);
        d.wait_busy();
        d.cmd(0xE9);
        d.data(&[0x01]);
        d.wait_busy();
        d.cmd(0x10);
        let mut frame = vec![0x55u8; RAM_BYTES];
        frame[0] = 0b00_01_10_11; // black, white, yellow, red
        d.data(&frame);
        d.cmd(0x04);
        d.wait_busy();
        d.cmd(0x12);
        d.data(&[0x00]);
        assert!(!p.busy_level(), "refresh asserts BUSY (emini checks this)");
        assert_eq!(p.visible()[0], 0b00_01_10_11);
        assert_eq!(p.visible()[1], 0x55);
        assert_eq!(p.version(), 1);
    }

    #[test]
    fn unpowered_or_reset_controller_ignores_traffic_and_keeps_the_image() {
        let mut p = powered(PanelVariant::Bwry);
        let mut d = Driver(&mut p);
        d.wait_busy();
        d.cmd(0x10);
        d.data(&vec![0x00; RAM_BYTES]);
        d.cmd(0x12);
        d.data(&[0x00]);
        assert_eq!(p.visible()[0], 0x00);
        p.set_power(false);
        assert!(p.busy_level(), "pulled-up BUSY when unpowered");
        p.set_dc(false);
        p.spi(&[0x12], 0);
        assert_eq!(p.diagnostics, [Diagnostic::UnpoweredTraffic { bytes: 1 }]);
        assert_eq!(
            p.visible()[0],
            0x00,
            "e-paper retains the image without power"
        );
    }

    #[test]
    fn busy_timing_is_virtual() {
        let mut p = powered(PanelVariant::Mono);
        assert_eq!(p.next_deadline(), Some(2 * MS));
        p.advance_to(MS);
        assert!(!p.busy_level());
        p.advance_to(2 * MS);
        assert!(p.busy_level());
        assert_eq!(p.next_deadline(), None);
    }

    #[test]
    fn external_waveform_is_reported_not_guessed() {
        let mut p = powered(PanelVariant::Mono);
        let mut d = Driver(&mut p);
        d.wait_busy();
        d.cmd(0x20);
        d.data(&[0u8; 535]);
        d.cmd(0x10);
        d.data(&vec![0x00; RAM_BYTES]);
        d.cmd(0x12);
        d.data(&[0x00]);
        assert_eq!(p.version(), 0, "visible image unchanged");
        assert_eq!(
            p.diagnostics,
            [Diagnostic::UnknownPanelWaveform { lut_len: 535 }]
        );
    }

    /// (pass, code) for levels 0..14, independent of `gray16::level_of`. Level 15 is code 0.
    fn ram_code(level: u8, pass: usize) -> u8 {
        const MAP: [(usize, u8); 15] = [
            (0, 1),
            (0, 2),
            (0, 3),
            (1, 1),
            (1, 2),
            (1, 3),
            (2, 1),
            (2, 2),
            (2, 3),
            (3, 1),
            (3, 2),
            (3, 3),
            (4, 1),
            (4, 2),
            (4, 3),
        ];
        if level >= 15 {
            return 0;
        }
        let (p, code) = MAP[level as usize];
        if p == pass {
            code
        } else {
            0
        }
    }

    /// OTP white, then the five external passes the demo sends (`0x20` + full RAM + `0x12`).
    fn note4_gray16(p: &mut Ssd2683, levels: &[u8]) {
        note4_full_1bpp(p, &vec![0xff; 50 * HEIGHT]);
        let mut d = Driver(p);
        for pass in 0..5 {
            let wf = crate::gray16::waveform(pass);
            if pass == 0 {
                d.0.set_reset(false);
                d.0.set_reset(true);
                d.wait_busy();
                d.cmd(0x00);
                d.data(&[0x2f, 0x8e]);
            } else {
                d.cmd(0x30);
                d.data(&[wf[6]]);
            }
            d.cmd(0x20);
            d.data(wf);
            d.cmd(0x10);
            d.wait_busy();
            let mut frame = vec![0u8; RAM_BYTES];
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let code = ram_code(levels[y * WIDTH + x], pass);
                    let shift = 6 - 2 * (x % 4);
                    frame[y * RAM_STRIDE + x / 4] |= code << shift;
                }
            }
            d.data(&frame);
            if pass == 0 {
                d.cmd(0x04);
                d.wait_busy();
            }
            d.cmd(0x12);
            d.data(&[0x00]);
            d.wait_busy();
        }
    }

    #[test]
    fn note4_gray16_ramp_matches_expected_pixels() {
        let mut levels = vec![0u8; WIDTH * HEIGHT];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                levels[y * WIDTH + x] = (x / 25) as u8; // 16 bands of 25 columns
            }
        }
        let mut p = powered(PanelVariant::Mono);
        note4_gray16(&mut p, &levels);
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
        assert_eq!(
            p.refreshes
                .iter()
                .filter(|r| r.kind == RefreshKind::Gray)
                .count(),
            5
        );
        assert_eq!(p.refreshes.last().unwrap().rect, (0, 0, 400, 300));
        for level in 0..16u8 {
            let x = level as usize * 25;
            assert_eq!(gray_at(&p, x, 0), level);
            assert_eq!(gray_at(&p, x + 24, 299), level);
            assert_eq!(crate::gray16::gray4_luma(gray_at(&p, x, 150)), level * 17);
        }
        assert_eq!(gray_at(&p, 0, 0) & 0xf0, 0, "high nibble is the left pixel");
    }

    #[test]
    fn note4_gray16_reference_pixels_are_exact() {
        let mut levels = vec![15u8; WIDTH * HEIGHT];
        levels[0] = 0;
        levels[1] = 7;
        levels[2] = 14;
        levels[WIDTH] = 3;
        levels[399] = 8;
        levels[WIDTH * HEIGHT - 1] = 1;
        let mut p = powered(PanelVariant::Mono);
        note4_gray16(&mut p, &levels);
        assert_eq!(gray_at(&p, 0, 0), 0);
        assert_eq!(gray_at(&p, 1, 0), 7);
        assert_eq!(gray_at(&p, 2, 0), 14);
        assert_eq!(gray_at(&p, 3, 0), 15, "unpainted neighbour stays white");
        assert_eq!(gray_at(&p, 0, 1), 3);
        assert_eq!(gray_at(&p, 399, 0), 8);
        assert_eq!(gray_at(&p, 399, 299), 1);
        assert_eq!(p.version(), 6, "OTP white plus five gray passes");
    }

    #[test]
    fn a_lut_that_is_not_a_render_pass_does_not_paint() {
        let mut p = powered(PanelVariant::Mono);
        note4_full_1bpp(&mut p, &vec![0xff; 50 * HEIGHT]);
        let before = p.visible().to_vec();
        let mut broken = crate::gray16::waveform(0).to_vec();
        broken[40] ^= 0x01;
        let mut d = Driver(&mut p);
        d.cmd(0x20);
        d.data(&broken);
        d.cmd(0x10);
        d.data(&vec![0x00; RAM_BYTES]);
        d.cmd(0x12);
        d.data(&[0x00]);
        assert_eq!(p.visible(), before.as_slice());
        assert!(p
            .diagnostics
            .iter()
            .any(|d| matches!(d, Diagnostic::UnknownPanelWaveform { lut_len: 535 })));
    }

    #[test]
    fn short_frame_keeps_unwritten_ram_overflow_is_diagnosed_and_reset_retains() {
        let mut p = powered(PanelVariant::Mono);
        let mut d = Driver(&mut p);
        d.wait_busy();
        d.cmd(0x10);
        d.data(&[0x00]); // four black pixels; the rest of RAM stays 0x55 (white)
        d.cmd(0x12);
        d.data(&[0x00]);
        assert_eq!(gray_at(&p, 0, 0), 0);
        assert_eq!(gray_at(&p, 3, 0), 0);
        assert_eq!(
            gray_at(&p, 4, 0),
            15,
            "short frame does not touch the exterior"
        );
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);

        let mut p = powered(PanelVariant::Mono);
        let mut d = Driver(&mut p);
        d.wait_busy();
        d.cmd(0x10);
        d.data(&vec![0x00; RAM_BYTES + 1]);
        assert_eq!(p.diagnostics, [Diagnostic::RamOverflow { command: 0x10 }]);

        let image = p.visible().to_vec();
        p.set_reset(false);
        p.set_reset(true);
        assert_eq!(
            p.visible(),
            image.as_slice(),
            "reset keeps the e-paper image"
        );
        assert!(!p.busy_level(), "leaving reset asserts BUSY");
        p.set_power(false);
        assert!(p.busy_level(), "power-off releases BUSY");
        assert_eq!(p.visible(), image.as_slice());
    }

    #[test]
    fn deep_sleep_ignores_commands_until_reset_and_refresh_busy_is_virtual() {
        let mut p = powered(PanelVariant::Mono);
        {
            let mut d = Driver(&mut p);
            d.wait_busy();
            d.cmd(0x07);
            d.data(&[0xa5]);
            d.cmd(0x10);
            d.data(&vec![0x00; RAM_BYTES]);
            d.cmd(0x12);
            d.data(&[0x00]);
        }
        assert_eq!(p.version(), 0, "deep sleep does not refresh");
        assert_eq!(gray_at(&p, 0, 0), 15);
        p.set_reset(false);
        p.set_reset(true);
        {
            let mut d = Driver(&mut p);
            d.wait_busy();
            d.cmd(0x12);
            d.data(&[0x00]);
        }
        assert!(!p.busy_level(), "refresh asserts BUSY");
        let until = p.next_deadline().expect("refresh deadline");
        p.advance_to(until);
        assert!(p.busy_level(), "BUSY releases at the deadline");
        assert_eq!(p.version(), 1);
    }
}
