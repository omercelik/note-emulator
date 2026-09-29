//! A NOTE device on the esp32sim engine: an ESP32-S3 with the NOTE board attached, booted
//! from the real mask ROM. The runtime drives it in slices of virtual time.

use std::net::{SocketAddr, TcpListener};

use esp_soc::{SocBus, Stop};
use note_core::flash::FlashImage;
use note_core::Profile;

use crate::board::{BoardHandle, NoteBoard};
use crate::replay::Guest;
use crate::snapshot::{self, Checkpoint, ColdBoot, HostLease, QuickBootImage, RestoreReport, SnapError};

pub const CPU_HZ: u64 = esp32s3::periph::CPU_HZ;
/// GPIO strapping as the ROM reads it: GPIO0 high (SPI boot) and low (UART/USB download).
pub const STRAP_SPI_BOOT: u32 = 0x08;
pub const STRAP_DOWNLOAD: u32 = 0x00;
/// Longest virtual time the SoC keeps running after its supply drops (1 ms).
const POWER_CHECK_CYCLES: u64 = CPU_HZ / 1000;

/// Why a slice ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SliceEnd {
    /// The requested virtual time was reached.
    Reached,
    /// The chip reset itself (software reset, watchdog) and came back up through the ROM.
    Rebooted { cause: u32, name: &'static str },
    /// Execution cannot continue (unimplemented instruction, unhandled trap).
    Stopped(String),
    /// The battery latch is open and there is no external power. Not a chip reset:
    /// `reboots` is unchanged and the e-paper image is kept.
    PoweredOff,
    /// A debugger breakpoint or single-step stopped the cores (see `gdb`).
    DebugStop,
}

/// Raw console bytes by channel, captured before any parsing or merging (Spec §11.2).
#[derive(Default, Debug)]
pub struct Console {
    pub uart0: Vec<u8>,
    pub usb: Vec<u8>,
}

/// Engine knobs the runtime needs without depending on the engine crates directly.
pub trait SocBusExt {
    fn set_log_unknown(&mut self, on: bool);
}

impl SocBusExt for esp32s3::bus::SocBus {
    fn set_log_unknown(&mut self, on: bool) {
        self.misc().log_unknown = on;
    }
}

/// The virtual radio environment. esp32sim's PHY calibration completes only when a virtual
/// access point exists, so one is always present; `nat` decides whether guest-initiated
/// traffic may reach the host network (off = no outbound host access, Spec §8.8).
/// Inbound listeners are separate and do not turn outbound on.
#[derive(Clone, Debug)]
pub struct RadioConfig {
    /// esp32sim AP spec: `ssid=NAME[,psk=PASS][,chan=N]`.
    pub ap: String,
    /// Outbound user-mode NAT. Inbound forwards can be added later without this.
    pub nat: bool,
}

impl Default for RadioConfig {
    fn default() -> Self {
        RadioConfig {
            ap: "ssid=esp32sim".into(),
            nat: false,
        }
    }
}

pub struct NoteMachine {
    pub m: esp32s3::Machine,
    pub board: BoardHandle,
    profile: Profile,
    mac: [u8; 6],
    pub reboots: u64,
    /// Cold power-ons after a supply loss (a subset of `reboots`).
    pub power_cycles: u64,
    uart0_queue: std::collections::VecDeque<u8>,
    /// The mask ROM image, re-applied on EN and power-on resets: its RAM-resident tables
    /// (`.data.interface.*`) are not restored by the ROM's own reset handler in the model.
    rom_elf: std::sync::Arc<Vec<u8>>,
    paused: bool,
    /// Bytes injected with `console.write`, per channel. The chip has one serial
    /// input, so both channels are also delivered there.
    input: [Vec<u8>; 2],
    /// Canonical panel pixels. Refreshed on advance and button changes so `Guest::frame`
    /// can return a reference the session holds across the pull.
    frame_cache: Vec<u8>,
    /// True while `run_until` is on the stack. Snapshots are refused here: `Machine::run`
    /// is the quantum boundary, and it does not return mid-instruction.
    pub(crate) in_slice: bool,
    state_epoch: u64,
    reconnect_pending: bool,
    pub(crate) button_queue: Vec<snapshot::QueuedButton>,
    host_endpoints: Vec<HostLease>,
    quick_boot: QuickBootImage,
    /// Identity of the installed firmware for snapshots (the AVD's base image hash).
    firmware_hash: [u8; 32],
    /// Legacy console frames (`HOME_EMULATOR` builds), per channel, and the last one decoded.
    legacy: [crate::legacy::LegacyConsole; 2],
    legacy_frame: Option<Vec<u8>>,
    legacy_version: u64,
    /// (speaker sample index, PA on) at each amplifier change.
    speaker_gate: Vec<(u64, bool)>,
    /// Host entropy for the hardware RNG: reseeded on every boot. None: the deterministic model.
    entropy: bool,
}

/// Adapter state in a snapshot's `NOTE` section.
#[derive(serde::Serialize, serde::Deserialize)]
struct NoteState {
    reboots: u64,
    power_cycles: u64,
    uart0_queue: std::collections::VecDeque<u8>,
    /// The display of a legacy build that draws over the console.
    legacy_frame: Option<Vec<u8>>,
    legacy_version: u64,
}

impl NoteMachine {
    pub fn new(
        profile: &Profile,
        rom_elf: &[u8],
        flash: &FlashImage,
        mac: [u8; 6],
        rtc_base_unix: i64,
        radio: &RadioConfig,
    ) -> Result<NoteMachine, String> {
        let flash_bytes = profile.flash_mib as usize * 1024 * 1024;
        if flash.bytes.len() != flash_bytes {
            return Err(format!(
                "flash image is {} bytes; profile {} has {flash_bytes}",
                flash.bytes.len(),
                profile.id
            ));
        }
        let mut m = esp32s3::machine(mac);
        let (board, handle) = NoteBoard::new(profile, rtc_base_unix);
        m.bus.board = Box::new(board);
        m.bus.attach_board_devices();
        let ap = esp32s3::wifi::ApConfig::parse(&radio.ap).map_err(|e| format!("radio: {e}"))?;
        m.bus.periph.wifi.ap = Some(esp32s3::wifi::VirtualAp::new(ap, debug_env("ESP_EMU_DEBUG_WIFI_FRAMES")));
        // Joins a SoftAP the firmware starts. Idle until the guest beacons.
        m.bus.periph.wifi.peer = Some(esp32s3::wifi::StationPeer::new(softap_peer_mac(mac)));
        if std::env::var("ESP_EMU_DEBUG").ok().is_some_and(|v| {
            v.split([',', ' '])
                .any(|a| a == "wifi-frames" || a == "all")
        }) {
            m.bus.periph.wifi.log = true;
            if let Some(ap) = &mut m.bus.periph.wifi.ap {
                ap.log = true;
            }
        }
        let mut net = esp32s3::net::VirtualNet::new(debug_env("ESP_EMU_DEBUG_NET"));
        if radio.nat {
            net.nat = Some(esp32s3::nat::Nat::new(debug_env("ESP_EMU_DEBUG_NET")));
        }
        m.bus.periph.wifi.net = Some(net);
        m.bus.refresh_tick_budget();
        m.bus.set_flash_size(flash_bytes);
        m.bus
            .set_psram_size(profile.psram.mib as usize * 1024 * 1024)?;
        m.load_rom(rom_elf)?;
        if let Ok(path) = std::env::var("ESP_EMU_TRACE_ELF") {
            let bytes = std::fs::read(&path).map_err(|e| format!("trace elf {path}: {e}"))?;
            m.add_symbols(&bytes)?;
            if let Ok(spec) = std::env::var("ESP_EMU_TRACE_FN") {
                for pat in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    let n = m.trace_fns(pat);
                    eprintln!("[note-emu] trace {pat} -> {n}");
                }
            }
        }
        m.write_flash(0, &flash.bytes)?;
        m.console.capture = true;
        // Real NOTE boards strap GPIO45/46 low: the ROM prints `boot:0x8 (SPI_FAST_FLASH_BOOT)`.
        m.bus.set_strap(STRAP_SPI_BOOT);
        m.boot_rom();
        let mut machine = NoteMachine {
            m,
            board: handle,
            profile: profile.clone(),
            mac,
            reboots: 0,
            power_cycles: 0,
            uart0_queue: Default::default(),
            rom_elf: std::sync::Arc::new(rom_elf.to_vec()),
            paused: false,
            input: [Vec::new(), Vec::new()],
            frame_cache: Vec::new(),
            in_slice: false,
            state_epoch: 1,
            reconnect_pending: false,
            button_queue: Vec::new(),
            host_endpoints: Vec::new(),
            quick_boot: QuickBootImage::default(),
            firmware_hash: [0; 32], legacy: Default::default(), legacy_frame: None, legacy_version: 0, speaker_gate: Vec::new(), entropy: false,
        };
        machine.apply_board_inputs();
        machine.refresh_frame();
        Ok(machine)
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    pub fn cycles(&self) -> u64 {
        self.m.bus.cycles()
    }

    pub fn seconds(&self) -> f64 {
        self.cycles() as f64 / CPU_HZ as f64
    }

    /// Drive every board-owned input line (buttons, charger, BUSY) onto the GPIO matrix.
    fn apply_board_inputs(&mut self) {
        let levels = self.m.bus.board.input_levels();
        for (pin, level) in levels {
            self.m.bus.periph.gpio.set_input(pin, level);
        }
        self.m.sync_irq();
    }

    /// Run until virtual time reaches `target_cycles`, rebooting through the ROM on chip resets.
    pub fn run_until(&mut self, target_cycles: u64) -> SliceEnd {
        self.cold_boot_if_pending();
        self.in_slice = true;
        let end = self.run_until_at_boundary(target_cycles);
        self.in_slice = false;
        self.apply_queued_buttons();
        end
    }

    fn run_until_at_boundary(&mut self, target_cycles: u64) -> SliceEnd {
        loop {
            if !self.board.lock().soc_powered {
                return SliceEnd::PoweredOff;
            }
            // The board sees a rail drop (latch opened, no external supply) as a GPIO edge; the
            // SoC stops within one power-check interval of virtual time, whatever slice the
            // caller asked for.
            let step_end = target_cycles.min(self.cycles().saturating_add(POWER_CHECK_CYCLES));
            self.m.max_cycles = step_end;
            let stop = self.m.run(u64::MAX);
            self.feed_uart0();
            self.note_speaker_gate();
            if !self.board.lock().soc_powered {
                return SliceEnd::PoweredOff;
            }
            if self.cycles() >= step_end && matches!(stop, Stop::Halted) {
                if self.cycles() >= target_cycles {
                    return SliceEnd::Reached;
                }
                continue;
            }
            match stop {
                Stop::SwReset => {
                    let cause = self.m.bus.reset_cause();
                    // The MAC registers are silicon and are rebuilt by `reboot`. The virtual AP,
                    // the SoftAP peer, NAT and the browser relay are the host's radio: they have
                    // to still be there or stored NVS settings have nothing to join.
                    let radio = take_host_radio(&mut self.m.bus.periph.wifi);
                    self.m.reboot();
                    restore_host_radio(&mut self.m.bus.periph.wifi, radio);
                    self.reboots += 1;
                    self.apply_entropy();
                    self.apply_board_inputs();
                    return SliceEnd::Rebooted {
                        cause,
                        name: esp_periph::reset_cause_name(cause),
                    };
                }
                Stop::Breakpoint(_) => return SliceEnd::DebugStop,
                Stop::Halted | Stop::MaxInsns => continue,
                other => return SliceEnd::Stopped(format!("{other:?}")),
            }
        }
    }

    /// Record amplifier (PA) on/off changes against the speaker sample index, at the 1 ms
    /// power-check resolution, so samples played while the PA was off reach the host as silence.
    fn note_speaker_gate(&mut self) {
        let amp = self.profile.audio.amp_gpio as usize;
        let on = self.board.lock().outputs.get(amp).copied().flatten() == Some(true);
        if self.speaker_gate.last().map(|g| g.1) != Some(on) {
            let at = self.m.bus.periph.audio().pcm.len() as u64;
            self.speaker_gate.push((at, on));
            if self.speaker_gate.len() > 4096 {
                self.speaker_gate.drain(..2048);
            }
        }
    }

    /// Whether the PA was on for speaker sample `index` (before the first record: off).
    fn speaker_on(&self, index: u64) -> bool {
        self.speaker_gate.iter().rev().find(|g| g.0 <= index).map(|g| g.1).unwrap_or(false)
    }

    /// A rail that died and came back (USB, or the power button closing the latch) is one
    /// cold boot. The latch-open edge itself does not call this.
    fn cold_boot_if_pending(&mut self) {
        let pending = {
            let mut st = self.board.lock();
            if !st.power_on_pending {
                return;
            }
            st.power_on_pending = false;
            true
        };
        if pending {
            self.power_on_reset();
        }
    }

    /// A cold power-on after the supply died: volatile memories and the RTC domain are lost,
    /// the ROM reports POWERON, flash and eFuse survive. The host radio (virtual AP, SoftAP peer,
    /// NAT, relays) is the host's and stays.
    fn power_on_reset(&mut self) {
        let radio = take_host_radio(&mut self.m.bus.periph.wifi);
        self.m.bus.clear_volatile_memory();
        self.m.bus.periph.rtc.reset_cause = esp_periph::rtc_cntl::RST_POWERON;
        self.m.reboot();
        self.reload_rom();
        self.apply_entropy();
        let sar = self.m.bus.periph.rtc.sar.take();
        self.m.bus.periph.rtc = esp_periph::rtc_cntl::RtcCntl::new();
        self.m.bus.periph.rtc.sar = sar;
        restore_host_radio(&mut self.m.bus.periph.wifi, radio);
        self.reboots += 1;
        self.power_cycles += 1;
        self.apply_board_inputs();
    }

    fn reload_rom(&mut self) {
        if !self.rom_elf.is_empty() {
            let rom = self.rom_elf.clone();
            if let Err(e) = self.m.load_rom(&rom) {
                eprintln!("[note] reloading the mask ROM after reset: {e}");
            }
        }
    }

    /// A reset through the EN pin (the auto-reset circuit of a USB-serial bridge): the chip
    /// restarts with the boot mode the GPIO0 strap selects; the ROM reports POWERON. Flash,
    /// eFuse and the host radio are kept.
    pub fn pin_reset(&mut self, download: bool) {
        let radio = take_host_radio(&mut self.m.bus.periph.wifi);
        self.m.bus.set_strap(if download { STRAP_DOWNLOAD } else { STRAP_SPI_BOOT });
        self.m.bus.periph.rtc.reset_cause = esp_periph::rtc_cntl::RST_POWERON;
        self.m.reboot();
        self.reload_rom();
        self.apply_entropy();
        restore_host_radio(&mut self.m.bus.periph.wifi, radio);
        self.reboots += 1;
        self.apply_board_inputs();
    }

    /// Close the battery latch after a power-off. Cold-boots once; does not wipe the e-paper image.
    pub fn wake(&mut self) {
        self.board.lock().close_battery_latch();
        self.cold_boot_if_pending();
    }

    /// USB/external supply. While it is present, opening the battery latch does not power the SoC off.
    /// Seed the hardware RNG from host entropy now and after every reset, like radio noise on
    /// silicon. Off (the default), `esp_random` is the same sequence on every boot, which keeps
    /// runs reproducible but repeats things like pairing codes.
    pub fn set_host_entropy(&mut self, on: bool) {
        self.entropy = on;
        self.apply_entropy();
    }

    fn apply_entropy(&mut self) {
        if !self.entropy {
            return;
        }
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(self.cycles() ^ self.reboots);
        self.m.bus.periph.wdev.reseed(h.finish());
    }

    /// A USB host on the S3's USB-Serial/JTAG (SOF every millisecond). On by default: runs start
    /// as if a debug cable and host were attached, so the USB console works. Off is a board on
    /// battery with no cable: `usb_serial_jtag_is_connected()` goes false after the IDF monitor
    /// timeout and firmware may release its power locks (and drops USB console output).
    pub fn set_usb_host(&mut self, attached: bool) {
        self.m.bus.periph.usb.connected = attached;
    }

    pub fn set_external_power(&mut self, on: bool) {
        self.board.lock().set_external_power(on);
        self.cold_boot_if_pending();
    }

    /// Present charger pins the guest reads. `full` is ignored as a level when the profile
    /// has not verified that pin's polarity.
    pub fn set_charger(&mut self, charging: bool, full: Option<bool>) -> crate::board::ChargeSense {
        let sense = self.board.lock().set_charger(charging, full);
        self.apply_board_inputs();
        sense
    }

    pub fn set_clock_policy(&mut self, policy: crate::pcf8563::ClockPolicy) {
        self.board.lock().rtc.set_policy(policy);
    }

    /// Console output since the last call. Legacy frame blocks are taken out (and shown as the
    /// display while the panel has never refreshed).
    pub fn take_console(&mut self) -> Console {
        let c = &mut self.m.console;
        c.all.clear();
        let (uart0, usb) = (std::mem::take(&mut c.uart0), std::mem::take(&mut c.usb));
        let console = Console { uart0: self.legacy[0].filter(&uart0), usb: self.legacy[1].filter(&usb) };
        let want = self.board.lock().panel.visible().len();
        let (width, height) = (self.profile.display.width as usize, self.profile.display.height as usize);
        for decoder in &self.legacy {
            if let Some((w, h, bytes)) = &decoder.frame {
                // pal2 frames of this panel only; the twin on the other channel is the same frame.
                if (*w, *h) == (width, height) && bytes.len() == want && self.legacy_frame.as_ref() != Some(bytes) {
                    self.legacy_frame = Some(bytes.clone());
                    self.legacy_version += 1;
                }
            }
        }
        console
    }

    /// What the display shows: the panel, or the legacy console frame for a build that never
    /// drives the panel.
    pub fn display_frame(&self) -> Vec<u8> {
        let st = self.board.lock();
        match &self.legacy_frame {
            Some(frame) if st.panel.refreshes.is_empty() => frame.clone(),
            _ => st.panel.visible().to_vec(),
        }
    }

    /// Changes whenever `display_frame` may have changed.
    pub fn display_version(&self) -> u64 {
        self.board.lock().panel.version() + self.legacy_version
    }

    /// Protocol frame source: 1 panel, 2 legacy console.
    pub fn display_source(&self) -> u8 {
        if self.legacy_frame.is_some() && self.board.lock().panel.refreshes.is_empty() { 2 } else { 1 }
    }

    /// Press (`down = true`) or release a profile button. Takes effect at the current cycle.
    pub fn set_button(&mut self, id: &str, down: bool) -> Result<(), String> {
        let button = self
            .profile
            .buttons
            .iter()
            .find(|b| b.id == id)
            .ok_or_else(|| format!("no button {id:?}"))?;
        let level = if button.active_low { !down } else { down };
        {
            let mut st = self.board.lock();
            match st.inputs.iter_mut().find(|(pin, _)| *pin == button.gpio) {
                Some(entry) => entry.1 = level,
                None => st.inputs.push((button.gpio, level)),
            }
        }
        self.m.bus.periph.gpio.set_input(button.gpio, level);
        self.m.sync_irq();
        Ok(())
    }

    /// Enable or disable the AArch64 JIT on both cores (the interpreter is the oracle).
    pub fn set_jit(&mut self, on: bool) {
        use emu_core::Core;
        for core in &mut self.m.cores {
            core.set_jit(on);
        }
    }

    /// Bytes for the USB-Serial/JTAG console's receive side.
    pub fn serial_input(&mut self, bytes: &[u8]) {
        self.m.bus.serial_input(bytes);
    }

    /// Bytes for UART0's receive side (RFC 2217 bridge, esptool). The 128-byte hardware FIFO is
    /// topped up from a host queue as the guest drains it, like a flow-controlled line, so
    /// nothing is dropped.
    pub fn uart0_input(&mut self, bytes: &[u8]) {
        self.uart0_queue.extend(bytes);
        self.feed_uart0();
    }

    /// Bytes still waiting in the host queue for UART0.
    pub fn uart0_backlog(&self) -> usize {
        self.uart0_queue.len()
    }

    fn feed_uart0(&mut self) {
        const FIFO: usize = 128;
        let room = FIFO.saturating_sub(self.m.bus.periph.uart[0].rx_pending());
        if room == 0 || self.uart0_queue.is_empty() {
            return;
        }
        let n = room.min(self.uart0_queue.len());
        let chunk: Vec<u8> = self.uart0_queue.drain(..n).collect();
        esp_soc::SocBus::uart_input(&mut self.m.bus, 0, &chunk);
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// Association responses the virtual AP has sent, and DHCP acknowledgements.
    /// A reconnect is a larger association count, not a repeated log line.
    pub fn station_net(&self) -> (u64, u64) {
        let associations = self.m.bus.periph.wifi.ap.as_ref().map(|ap| ap.associations).unwrap_or(0);
        let acks = self.m.bus.periph.wifi.net.as_ref().map(|net| net.dhcp_acks).unwrap_or(0);
        (associations, acks)
    }

    /// Deauthenticate the guest station. Its own driver has to authenticate and
    /// associate again; this does not rewrite guest NVS or application state.
    pub fn disconnect_station(&mut self) -> bool {
        let now_us = self.cycles() / (CPU_HZ / 1_000_000);
        match self.m.bus.periph.wifi.ap.as_mut() {
            Some(ap) => ap.disconnect(now_us),
            None => false,
        }
    }

    /// Make sure the inbound relay exists. Outbound stays as `--nat` left it.
    fn ensure_inbound(&mut self) {
        let net = self
            .m
            .bus
            .periph
            .wifi
            .net
            .as_mut()
            .expect("virtual network");
        if net.nat.is_none() {
            let mut nat = esp32s3::nat::Nat::new(debug_env("ESP_EMU_DEBUG_NET"));
            nat.outbound = false;
            net.nat = Some(nat);
        }
    }

    /// Listen on `bind` and inject a SYN toward the station's `guest_port` for each accept.
    pub fn listen_forward(
        &mut self,
        bind: SocketAddr,
        guest_port: u16,
    ) -> std::io::Result<SocketAddr> {
        self.ensure_inbound();
        let mac = self.mac;
        let addr = self.m.bus.periph.wifi.net.as_mut().expect("virtual network").listen_forward(bind, mac, guest_port)?;
        self.remember_endpoint(addr, guest_port, false);
        Ok(addr)
    }

    /// Forward a listener someone else bound (the helper's `192.168.4.1:80` fd).
    pub fn adopt_forward(
        &mut self,
        listener: TcpListener,
        guest_port: u16,
    ) -> std::io::Result<SocketAddr> {
        self.ensure_inbound();
        let mac = self.mac;
        let addr = self.m.bus.periph.wifi.net.as_mut().expect("virtual network").adopt_forward(listener, mac, guest_port)?;
        self.remember_endpoint(addr, guest_port, false);
        Ok(addr)
    }

    /// Drop inbound listeners and queue RSTs for their guest flows.
    pub fn close_forwards(&mut self) {
        let frames = self
            .m
            .bus
            .periph
            .wifi
            .net
            .as_mut()
            .map(|net| net.close_forwards())
            .unwrap_or_default();
        self.m.bus.periph.wifi.eth_rx.extend(frames);
        self.host_endpoints.retain(|lease| lease.softap);
    }

    /// Listen on `bind` and carry accepted TCP to the guest SoftAP's `guest_port`.
    /// The station's DHCP lease selects the source address; until then nothing is accepted.
    pub fn listen_softap(
        &mut self,
        bind: SocketAddr,
        guest_port: u16,
    ) -> std::io::Result<SocketAddr> {
        let mac = self.softap_mac();
        let relay = self.m.bus.periph.wifi.relay.get_or_insert_with(|| esp32s3::softap::SoftApRelay::new(mac));
        let addr = relay.listen(bind, guest_port)?;
        self.remember_endpoint(addr, guest_port, true);
        Ok(addr)
    }

    /// Same relay, using a listener the setup-address helper already bound.
    pub fn adopt_softap(
        &mut self,
        listener: std::net::TcpListener,
        guest_port: u16,
    ) -> std::io::Result<SocketAddr> {
        let mac = self.softap_mac();
        let relay = self.m.bus.periph.wifi.relay.get_or_insert_with(|| esp32s3::softap::SoftApRelay::new(mac));
        let addr = relay.adopt(listener, guest_port)?;
        self.remember_endpoint(addr, guest_port, true);
        Ok(addr)
    }

    /// What the emulated station has heard from a guest SoftAP, if anything.
    /// `(ssid, privacy, passphrase set, leased address)`.
    pub fn softap_sight(&self) -> Option<(String, bool, bool, Option<[u8; 4]>, &'static str)> {
        let peer = self.m.bus.periph.wifi.peer.as_ref()?;
        if peer.ssid.is_empty() {
            return None;
        }
        Some((
            peer.ssid.clone(),
            peer.privacy,
            peer.has_passphrase(),
            peer.ip,
            peer.phase(),
        ))
    }

    pub fn set_softap_passphrase(&mut self, psk: &str) -> bool {
        if let Some(peer) = &mut self.m.bus.periph.wifi.peer {
            peer.set_passphrase(psk);
            true
        } else {
            false
        }
    }

    /// Leased address, AP BSSID and SSID once the emulated station has joined a guest SoftAP.
    pub fn softap_client(&self) -> Option<([u8; 4], [u8; 6], String)> {
        let peer = self.m.bus.periph.wifi.peer.as_ref()?;
        Some((peer.ip?, peer.bssid, peer.ssid.clone()))
    }

    fn softap_mac(&self) -> [u8; 6] {
        self.m
            .bus
            .periph
            .wifi
            .peer
            .as_ref()
            .map(|peer| peer.mac)
            .unwrap_or(self.mac)
    }

    /// Drop one listener (the address `listen_forward` or `adopt_forward` returned).
    pub fn close_forward(&mut self, addr: SocketAddr) {
        let frames = self
            .m
            .bus
            .periph
            .wifi
            .net
            .as_mut()
            .map(|net| net.close_forward(addr))
            .unwrap_or_default();
        self.m.bus.periph.wifi.eth_rx.extend(frames);
        self.host_endpoints.retain(|lease| lease.addr != addr);
    }

    /// Guest-visible flash, including bytes the firmware has programmed.
    pub fn flash_contents(&self) -> &[u8] {
        &self.m.bus.flash
    }

    /// The next slice takes the software-reset path (`esp_restart`), keeping flash.
    pub fn request_reset(&mut self) {
        if !self.m.bus.periph.rtc.sw_reset {
            self.m.bus.periph.rtc.sw_reset = true;
            self.m.bus.periph.rtc.reset_cause = esp_periph::RST_SW_CPU;
        }
    }

    fn expect(&self, firmware_hash: [u8; 32]) -> snapshot::Identity {
        snapshot::identity_for(&self.profile, firmware_hash)
    }

    fn meta(&self, firmware_hash: [u8; 32]) -> snapshot::Meta {
        snapshot::Meta {
            identity: self.expect(firmware_hash),
            at_boundary: !self.in_slice,
            held: self.held_keys(),
            queue: self.button_queue.clone(),
            endpoints: self.host_endpoints.clone(),
        }
    }

    fn held_keys(&self) -> Vec<snapshot::HeldKey> {
        let st = self.board.lock();
        self.profile.buttons.iter().filter_map(|button| {
            let down = !button.active_low;
            let level = st.inputs.iter().find(|(pin, _)| *pin == button.gpio).map(|(_, level)| *level);
            (level == Some(down)).then(|| snapshot::HeldKey { id: button.id.clone(), gpio: button.gpio })
        }).collect()
    }

    fn remember_endpoint(&mut self, addr: SocketAddr, guest_port: u16, softap: bool) {
        self.host_endpoints.retain(|lease| lease.addr != addr);
        self.host_endpoints.push(HostLease { addr, guest_port, softap });
    }

    /// Full machine snapshot (schema 2), taken only between run slices. Code caches are not
    /// included. See `snapshot` for what each section holds.
    pub fn save_snapshot(&self, firmware_hash: [u8; 32]) -> Result<Vec<u8>, SnapError> {
        let meta = self.meta(firmware_hash);
        let board = emu_core::snap::to_bytes(&*self.board.lock()).map_err(|e| SnapError::State(e.to_string()))?;
        let note = emu_core::snap::to_bytes(&NoteState {
            reboots: self.reboots,
            power_cycles: self.power_cycles,
            uart0_queue: self.uart0_queue.clone(),
            legacy_frame: self.legacy_frame.clone(),
            legacy_version: self.legacy_version,
        })
        .map_err(|e| SnapError::State(e.to_string()))?;
        let image = snapshot::capture(&self.m, board, note, &meta)?;
        Ok(snapshot::encode(&image))
    }

    /// Restore a schema 2 snapshot. Identity, framing and every state section are checked;
    /// if any part does not apply, the machine is put back as it was. Held keys are released
    /// afterwards, guest network flows are closed, saved listeners are rebound (an occupied one
    /// is reported, not moved) and the Wi-Fi link is dropped so the guest reconnects.
    pub fn restore_snapshot(&mut self, bytes: &[u8], firmware_hash: [u8; 32]) -> Result<RestoreReport, SnapError> {
        if self.in_slice {
            return Err(SnapError::NotAtBoundary);
        }
        let image = snapshot::prepare(bytes, &self.expect(firmware_hash), &self.m)?;
        for key in &image.held {
            if !self.profile.buttons.iter().any(|button| button.id == key.id && button.gpio == key.gpio) {
                return Err(SnapError::Corrupt("button"));
            }
        }
        let note: NoteState = emu_core::snap::from_bytes(&image.note).map_err(|e| SnapError::State(format!("NOTE: {e}")))?;
        let state = |e: emu_core::snap::SnapError| SnapError::State(e.to_string());
        let backup_machine = self.m.save_state().map_err(state)?;
        let backup_board = emu_core::snap::to_bytes(&*self.board.lock()).map_err(state)?;
        let applied = self
            .m
            .restore_state(&image.machine)
            .and_then(|()| emu_core::snap::restore_in_place(&image.board, &mut *self.board.lock()));
        if let Err(e) = applied {
            // Put back what was there; both backups came from this machine a moment ago.
            let _ = self.m.restore_state(&backup_machine);
            let _ = emu_core::snap::restore_in_place(&backup_board, &mut *self.board.lock());
            return Err(SnapError::State(e.to_string()));
        }
        self.reboots = note.reboots;
        self.power_cycles = note.power_cycles;
        self.uart0_queue = note.uart0_queue;
        self.legacy_frame = note.legacy_frame;
        // Keep the version moving forward so a viewer notices the restored picture.
        self.legacy_version = self.legacy_version.max(note.legacy_version) + 1;
        self.button_queue = image.queue.clone();
        let released = self.release_held(&image.held);
        self.state_epoch = self.state_epoch.wrapping_add(1);
        let (occupied, host_sockets) = self.reconnect_endpoints(&image.endpoints);
        let link_dropped = self.disconnect_station();
        self.reconnect_pending = link_dropped;
        self.apply_queued_buttons();
        self.refresh_frame();
        Ok(RestoreReport {
            partial: false,
            released,
            epoch: self.state_epoch,
            reconnect_pending: link_dropped,
            occupied,
            host_sockets,
            full_frame: true,
        })
    }

    fn release_held(&mut self, held: &[snapshot::HeldKey]) -> Vec<String> {
        let mut released = Vec::new();
        for key in held {
            if self.set_button(&key.id, false).is_ok() {
                released.push(key.id.clone());
            }
        }
        released
    }

    /// Close class-X host sockets and try the saved leases again. RST frames from the
    /// NAT are dropped: schema 1 does not restore guest TCP sequence numbers.
    fn reconnect_endpoints(&mut self, endpoints: &[HostLease]) -> (Vec<String>, usize) {
        self.close_host_sockets();
        self.host_endpoints.clear();
        let mut occupied = Vec::new();
        for lease in endpoints {
            if snapshot::forbidden_port(lease.addr.port()) {
                occupied.push(lease.addr.to_string());
                continue;
            }
            let rebound = if lease.softap {
                self.listen_softap(lease.addr, lease.guest_port)
            } else {
                self.listen_forward(lease.addr, lease.guest_port)
            };
            match rebound {
                Ok(bound) if bound == lease.addr => {}
                Ok(bound) => {
                    if !lease.softap {
                        self.close_forward(bound);
                    }
                    occupied.push(lease.addr.to_string());
                }
                Err(_) => occupied.push(lease.addr.to_string()),
            }
        }
        (occupied, snapshot::host_socket_count(&self.m))
    }

    fn close_host_sockets(&mut self) {
        if let Some(net) = self.m.bus.periph.wifi.net.as_mut() {
            let _frames = net.close_host_flows_for_restore();
        }
        if let Some(relay) = self.m.bus.periph.wifi.relay.as_mut() {
            let _frames = relay.on_snapshot_restore();
        }
        let wifi = &mut self.m.bus.periph.wifi;
        wifi.eth_tx.clear();
        wifi.eth_rx.clear();
        wifi.peer_eth.clear();
        wifi.peer_tx.clear();
    }

    #[cfg(test)]
    pub(crate) fn close_saved_endpoints(&mut self) {
        self.close_host_sockets();
        self.host_endpoints.clear();
    }

    pub fn queue_button(&mut self, gpio: u8, level: bool, at_cycle: u64) {
        self.button_queue.push(snapshot::QueuedButton { gpio, level, at_cycle });
    }

    pub fn apply_queued_buttons(&mut self) {
        let now = self.cycles();
        for ev in snapshot::take_due(&mut self.button_queue, now) {
            self.m.bus.periph.gpio.set_input(ev.gpio, ev.level);
            let mut st = self.board.lock();
            match st.inputs.iter_mut().find(|(pin, _)| *pin == ev.gpio) {
                Some(entry) => entry.1 = ev.level,
                None => st.inputs.push((ev.gpio, ev.level)),
            }
        }
    }

    /// Persistent bytes only. Labelled "Flash checkpoint"; loading it does not touch the CPU.
    pub fn save_flash_checkpoint(&self, firmware_hash: [u8; 32], persistent: &[u8]) -> Result<Vec<u8>, SnapError> {
        if self.in_slice {
            return Err(SnapError::NotAtBoundary);
        }
        Ok(snapshot::encode_checkpoint(&self.expect(firmware_hash), persistent))
    }

    pub fn load_flash_checkpoint(&self, bytes: &[u8], firmware_hash: [u8; 32], persistent: &mut Vec<u8>) -> Result<Checkpoint, SnapError> {
        let checkpoint = snapshot::read_flash_checkpoint(bytes, &self.expect(firmware_hash))?;
        *persistent = checkpoint.flash.clone();
        Ok(checkpoint)
    }

    pub fn store_quick_boot_image(&mut self, bytes: Vec<u8>) {
        self.quick_boot = QuickBootImage { bytes, failures: 0, disabled: false, last_error: String::new() };
    }

    /// Unavailable until SNAP-05 passes on NOTE4 and NOTE4C. Failure cold-boots
    /// and does not write `persistent`.
    pub fn attempt_quick_boot(&mut self, persistent: &[u8]) -> ColdBoot {
        snapshot::attempt_quick_boot(&mut self.quick_boot, persistent)
    }

    /// The installed firmware's identity; snapshots from other firmware are refused.
    pub fn set_firmware_hash(&mut self, hash: [u8; 32]) {
        self.firmware_hash = hash;
    }

    fn refresh_frame(&mut self) {
        self.frame_cache = self.display_frame();
    }
}

struct HostRadio {
    ap: Option<esp32s3::wifi::VirtualAp>,
    peer: Option<esp32s3::wifi::StationPeer>,
    net: Option<esp32s3::net::VirtualNet>,
    relay: Option<esp32s3::softap::SoftApRelay>,
    log: bool,
}

/// Lift the host radio off the MAC, drop the association the reset just killed,
/// and leave the passphrase and AP configuration in place.
fn take_host_radio(wifi: &mut esp32s3::periph::WifiMac) -> HostRadio {
    if let Some(ap) = &mut wifi.ap {
        ap.state = esp32s3::wifi::StaState::Idle;
        ap.wpa.state = esp32s3::wifi::WpaState::Idle;
        ap.sta = [0; 6];
        ap.queue.clear();
    }
    if let Some(peer) = &mut wifi.peer {
        peer.release_link();
    }
    if let Some(relay) = &mut wifi.relay {
        relay.set_lease([0; 4], [0; 6], [0; 4]);
    }
    HostRadio {
        ap: wifi.ap.take(),
        peer: wifi.peer.take(),
        net: wifi.net.take(),
        relay: wifi.relay.take(),
        log: wifi.log,
    }
}

fn restore_host_radio(wifi: &mut esp32s3::periph::WifiMac, radio: HostRadio) {
    wifi.ap = radio.ap;
    wifi.peer = radio.peer;
    wifi.net = radio.net;
    wifi.relay = radio.relay;
    wifi.log = radio.log;
}

/// Station that joins a SoftAP this chip starts. ESP-IDF's AP address is the
/// station address with the last octet incremented, so flipping only that bit
/// would make the peer the AP and the join would be ignored.
fn softap_peer_mac(guest: [u8; 6]) -> [u8; 6] {
    let mut mac = guest;
    mac[5] ^= 0x11;
    mac
}

fn ns_to_cycles(ns: u64) -> u64 {
    ((ns as u128) * CPU_HZ as u128 / 1_000_000_000) as u64
}

fn cycles_to_ns(cycles: u64) -> u64 {
    ((cycles as u128) * 1_000_000_000 / CPU_HZ as u128) as u64
}

impl Guest for NoteMachine {
    fn set_softap_passphrase(&mut self, psk: &str) -> bool {
        NoteMachine::set_softap_passphrase(self, psk)
    }
    fn profile_id(&self) -> &str {
        &self.profile.id
    }

    fn save_snapshot(&self) -> Result<Vec<u8>, String> {
        NoteMachine::save_snapshot(self, self.firmware_hash).map_err(|e| e.to_string())
    }

    fn restore_snapshot(&mut self, bytes: &[u8]) -> Result<serde_json::Value, String> {
        let r = NoteMachine::restore_snapshot(self, bytes, self.firmware_hash).map_err(|e| e.to_string())?;
        Ok(serde_json::json!({
            "partial": r.partial, "released": r.released, "reconnect_pending": r.reconnect_pending,
            "occupied": r.occupied, "host_sockets": r.host_sockets, "virtual_ns": self.now_ns(),
        }))
    }

    fn frame_source(&self) -> u8 {
        self.display_source()
    }

    fn pin_reset(&mut self, download: bool) -> Result<(), String> {
        NoteMachine::pin_reset(self, download);
        Ok(())
    }

    fn mic_wav(&mut self, wav: &[u8]) -> Result<usize, String> {
        // Speech replaces the speaker loopback; an empty queue stays silence, so a guest that
        // keeps its receiver running never stalls between clips.
        let mut queued = 0;
        for i2s in [&mut self.m.bus.periph.i2s0, &mut self.m.bus.periph.i2s1] {
            i2s.loopback = false;
            i2s.rx_idle_silence = true;
            queued = i2s.push_rx_wav(wav).map_err(str::to_string)?;
        }
        Ok(queued)
    }

    fn mic_listening(&self) -> bool {
        self.m.bus.periph.i2s0.rx_running() || self.m.bus.periph.i2s1.rx_running()
    }

    fn mic_pcm(&mut self, samples: &[i16], rate: u32) -> Result<usize, String> {
        if rate == 0 {
            return Err("rate must be positive".into());
        }
        let mut queued = 0;
        for i2s in [&mut self.m.bus.periph.i2s0, &mut self.m.bus.periph.i2s1] {
            if !i2s.rx_running() {
                continue;
            }
            i2s.loopback = false;
            i2s.rx_idle_silence = true;
            let target = i2s.derive_rx_rate().unwrap_or(rate);
            let resampled = resample_linear(samples, rate, target);
            let bytes: Vec<u8> = resampled.iter().flat_map(|s| s.to_le_bytes()).collect();
            i2s.push_rx_bytes(&bytes);
            queued = resampled.len();
        }
        Ok(queued)
    }

    fn set_usb(&mut self, cable: Option<bool>, host: Option<bool>) {
        if let Some(plugged) = cable {
            self.set_external_power(plugged);
            self.set_charger(plugged, None);
        }
        if let Some(attached) = host {
            self.set_usb_host(attached);
        }
    }

    fn usb(&self) -> (bool, bool) {
        (self.board.lock().external_power, self.m.bus.periph.usb.connected)
    }

    fn speaker(&self, from: Option<u64>, max: usize) -> (u32, u64, Vec<i16>) {
        let audio = self.m.bus.periph.audio();
        let total = audio.pcm.len() as u64;
        let start = from.unwrap_or(total).min(total).max(total.saturating_sub(max as u64));
        let mut samples = audio.pcm[start as usize..].to_vec();
        // The speaker only sounds through the amplifier: I2S data sent while its enable pin was
        // not driven high is silence at the speaker.
        // Then the codec's DAC volume and mute as the guest last programmed them (read-time).
        let gain = self.board.lock().codec.dac_gain();
        for (i, s) in samples.iter_mut().enumerate() {
            *s = if self.speaker_on(start + i as u64) { (*s as f32 * gain).clamp(-32768.0, 32767.0) as i16 } else { 0 };
        }
        (audio.sample_rate, start, samples)
    }

    fn leds(&self) -> Vec<(&'static str, bool)> {
        let st = self.board.lock();
        if st.has_led() { vec![("power", st.led_lit)] } else { Vec::new() }
    }

    fn engine(&self) -> &'static str {
        "esp32sim"
    }

    fn advance(&mut self, target_ns: u64) {
        if self.paused {
            return;
        }
        let target = ns_to_cycles(target_ns);
        while self.cycles() < target {
            match self.run_until(target) {
                SliceEnd::Reached => break,
                SliceEnd::Rebooted { .. } => continue,
                SliceEnd::Stopped(_) | SliceEnd::PoweredOff | SliceEnd::DebugStop => break,
            }
        }
        self.refresh_frame();
    }

    fn now_ns(&self) -> u64 {
        cycles_to_ns(self.cycles())
    }

    fn take_console(&mut self) -> Vec<(u8, Vec<u8>)> {
        let console = NoteMachine::take_console(self);
        let mut out = Vec::new();
        if !console.uart0.is_empty() {
            out.push((0, console.uart0));
        }
        if !console.usb.is_empty() {
            out.push((1, console.usb));
        }
        out
    }

    fn button(&mut self, id: &str, down: bool) -> Result<(), String> {
        self.set_button(id, down)?;
        self.refresh_frame();
        Ok(())
    }

    fn battery_mv(&self) -> u32 {
        self.board.lock().battery_mv
    }

    fn set_battery_mv(&mut self, mv: u32) {
        self.board.lock().set_battery_mv(mv);
    }

    fn frame(&self) -> &[u8] {
        &self.frame_cache
    }

    fn width(&self) -> u16 {
        self.profile.display.width as u16
    }

    fn height(&self) -> u16 {
        self.profile.display.height as u16
    }

    fn format(&self) -> u8 {
        match self.profile.display.format {
            note_core::DisplayFormat::Pal2 => 1,
            note_core::DisplayFormat::Gray4 => 2,
        }
    }

    fn console_input(&mut self, channel: u8, bytes: &[u8]) {
        if (channel as usize) < self.input.len() {
            self.input[channel as usize].extend_from_slice(bytes);
        }
        self.serial_input(bytes);
    }

    fn input(&self, channel: u8) -> &[u8] {
        self.input
            .get(channel as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    fn paused(&self) -> bool {
        self.paused
    }
}

#[cfg(test)]
impl NoteMachine {
    /// A machine with no mask ROM and no firmware. Snapshot tests use this.
    pub(crate) fn bare(profile: &note_core::Profile) -> NoteMachine {
        let mac = [0x02, 0x4e, 0x4f, 0x54, 0x45, 0x01];
        let mut m = esp32s3::machine(mac);
        let (board, handle) = NoteBoard::new(profile, 0);
        m.bus.board = Box::new(board);
        m.bus.attach_board_devices();
        m.bus.periph.wifi.net = Some(esp32s3::net::VirtualNet::new(false));
        let mut machine = NoteMachine {
            m, board: handle, profile: profile.clone(), mac, reboots: 0, power_cycles: 0, uart0_queue: Default::default(), rom_elf: Default::default(),
            paused: false, input: [Vec::new(), Vec::new()], frame_cache: Vec::new(),
            in_slice: false, state_epoch: 1, reconnect_pending: false,
            button_queue: Vec::new(), host_endpoints: Vec::new(), quick_boot: QuickBootImage::default(), firmware_hash: [0; 32], legacy: Default::default(), legacy_frame: None, legacy_version: 0, speaker_gate: Vec::new(), entropy: false,
        };
        machine.refresh_frame();
        machine
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_radio_survives_the_chip_reset_but_the_dead_link_does_not() {
        let mut wifi = esp32s3::periph::WifiMac::new();
        let mut ap = esp32s3::wifi::VirtualAp::new(
            esp32s3::wifi::ApConfig::parse("ssid=esp32sim,psk=station-secret").unwrap(),
            false,
        );
        ap.state = esp32s3::wifi::StaState::Associated;
        ap.sta = [0x02, 0x4e, 0x4f, 0x54, 0x45, 0x4c];
        wifi.ap = Some(ap);
        let mut peer = esp32s3::wifi::StationPeer::new([0x02, 0x4e, 0x4f, 0x54, 0x45, 0x5d]);
        peer.set_passphrase("softap-secret");
        wifi.peer = Some(peer);
        wifi.net = Some(esp32s3::net::VirtualNet::new(false));
        let mut relay = esp32s3::softap::SoftApRelay::new([0x02, 0x4e, 0x4f, 0x54, 0x45, 0x5d]);
        relay.set_lease([192, 168, 4, 2], [0x02, 0x11, 0x22, 0x33, 0x44, 0x55], [192, 168, 4, 1]);
        assert!(relay.leased());
        wifi.relay = Some(relay);
        wifi.log = true;

        let radio = take_host_radio(&mut wifi);
        wifi = esp32s3::periph::WifiMac::new();
        assert!(wifi.ap.is_none() && wifi.peer.is_none());
        restore_host_radio(&mut wifi, radio);

        let ap = wifi.ap.as_ref().unwrap();
        assert_eq!(ap.cfg.ssid, "esp32sim");
        assert_eq!(ap.cfg.psk.as_deref(), Some("station-secret"));
        assert_eq!(ap.state, esp32s3::wifi::StaState::Idle);
        assert_eq!(ap.sta, [0; 6]);
        let peer = wifi.peer.as_ref().unwrap();
        assert!(peer.has_passphrase());
        assert!(peer.ssid.is_empty());
        assert!(peer.ip.is_none());
        assert_eq!(peer.phase(), "idle");
        assert_eq!(peer.mac, [0x02, 0x4e, 0x4f, 0x54, 0x45, 0x5d]);
        assert!(wifi.net.is_some());
        assert!(!wifi.relay.as_ref().unwrap().leased());
        assert!(wifi.log);
    }

    #[test]
    fn softap_peer_is_neither_the_station_nor_the_esp_idf_ap() {
        for last in [0x04u8, 0x4c, 0x00, 0xff] {
            let guest = [0x02, 0x4e, 0x4f, 0x54, 0x45, last];
            let peer = softap_peer_mac(guest);
            let ap = {
                let mut mac = guest;
                mac[5] = mac[5].wrapping_add(1);
                mac
            };
            assert_ne!(peer, guest);
            assert_ne!(peer, ap);
        }
    }

    #[test]
    fn one_virtual_second_is_one_billion_nanoseconds() {
        assert_eq!(cycles_to_ns(CPU_HZ), 1_000_000_000);
        assert_eq!(ns_to_cycles(1_000_000_000), CPU_HZ);
    }
}

/// The engine's own network/radio trace switches (`ESP_EMU_DEBUG_NET=1`, ...), honoured for
/// NOTE machines too.
fn debug_env(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty() && v != "0")
}

/// Linear-interpolation resampling of mono i16 (host microphone to the guest's I2S rate).
fn resample_linear(samples: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to || samples.is_empty() {
        return samples.to_vec();
    }
    let out_len = (samples.len() as u64 * to as u64 / from as u64) as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * from as f64 / to as f64;
            let base = pos.floor() as usize;
            let frac = pos - base as f64;
            let a = samples[base.min(samples.len() - 1)] as f64;
            let b = samples[(base + 1).min(samples.len() - 1)] as f64;
            (a + (b - a) * frac).round() as i16
        })
        .collect()
}

#[cfg(test)]
mod resample_tests {
    use super::resample_linear;

    #[test]
    fn resampling_keeps_duration_and_shape() {
        let ramp: Vec<i16> = (0..480).map(|i| i as i16 * 10).collect();
        let down = resample_linear(&ramp, 48_000, 16_000);
        assert_eq!(down.len(), 160);
        assert_eq!(down[0], 0);
        assert_eq!(down[1], 30);
        assert_eq!(resample_linear(&ramp, 16_000, 16_000), ramp);
        assert_eq!(resample_linear(&ramp[..160], 16_000, 48_000).len(), 480);
    }
}
