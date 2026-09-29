//! `note-emu` — headless NOTE emulator host (proposal §8). G1 scope: boot a profile + firmware
//! on the Rust engine, capture raw console channels, export every panel refresh as PNG, and
//! drive buttons from a script. The AVD store, protocol server and pacing policies arrive in G3.

mod gdbserve;
mod serialpty;
mod serialserve;
use std::collections::HashMap;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Parser;
use note_core::flash::{FlashImage, Layout};
use note_core::{paths, profile, rom, DisplayFormat, Profile};
use note_machine::{Guest, NoteMachine, RadioConfig, SliceEnd, CPU_HZ};
use note_net_helper::{default_socket, probe_shared, request_shared, Session, SharedProbe};
use note_runtime::{incompatible_shared, parse_forward, shared_failure_permission, FrameCapture, HostMode, Instance, NetworkInfo, OwnedServer, Queued, Store};

#[derive(Parser, Clone)]
#[command(name = "note-emu", version, about = "Headless NOTE emulator host")]
struct Args {
    /// Hardware profile id (`ndb profiles`). Required unless `--avd` is set.
    #[arg(long, required_unless_present = "avd")]
    profile: Option<String>,
    /// Merged flash image or ESP-IDF build directory. Required unless `--avd` is set.
    #[arg(long, required_unless_present = "avd")]
    firmware: Option<PathBuf>,
    /// Boot a stored AVD and serve the control socket for `ndb -s`.
    #[arg(long)]
    avd: Option<String>,
    /// Stop after this much virtual time. `0` runs until a client sends `stop`.
    #[arg(long, default_value_t = 20.0)]
    seconds: f64,
    /// Directory for raw console captures and frame PNGs.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Write the committed panel timeline to panel.gif in --out.
    #[arg(long, requires = "out")]
    capture_gif: bool,
    /// Write captured speaker PCM to speaker.wav in --out.
    #[arg(long, requires = "out")]
    capture_wav: bool,
    /// Feed a PCM WAV to the I²S microphone input on both controllers.
    #[arg(long)]
    mic_wav: Option<PathBuf>,
    /// Button script, repeatable: `<seconds>:<button>[:<hold ms>]`, e.g. `6.5:ok` or `8:down:3200`.
    #[arg(long = "press")]
    presses: Vec<String>,
    /// Keep virtual time at or behind host time (needed for real networks); default runs flat out.
    #[arg(long)]
    realtime: bool,
    /// Print both console channels in full instead of collapsing cross-channel twins.
    #[arg(long)]
    raw_console: bool,
    /// Stop as soon as this many panel refreshes have happened.
    #[arg(long)]
    frames: Option<u64>,
    /// Log accesses to peripheral registers the engine does not model.
    #[arg(long)]
    log_periph: bool,
    /// Print CPU registers and the board report at the end.
    #[arg(long)]
    dump: bool,
    /// Virtual access point the firmware can see: `ssid=NAME[,psk=PASS][,chan=N]`.
    #[arg(long, default_value = "ssid=esp32sim")]
    wifi: String,
    /// Take the virtual AP's SSID and passphrase from `WIFI_SSID` / `WIFI_PASSWORD` in a mode-0600
    /// dotenv file (a firmware project's `.env`), so they stay off the command line. Other keys
    /// are ignored and nothing from the file is logged. Replaces `--wifi`.
    #[arg(long, value_name = "FILE")]
    wifi_env: Option<PathBuf>,
    /// Let guest-initiated traffic reach the host network through NAT.
    /// Off by itself: no outbound host access. Inbound forwards do not turn this on.
    #[arg(long)]
    nat: bool,
    /// Forward a loopback port to a guest TCP port. Repeatable.
    /// `8080`, `8080:80`, or `127.0.0.1:8080:80`. A busy port picks another and reports it.
    #[arg(long = "forward")]
    forwards: Vec<String>,
    /// Lease `http://192.168.4.1/` from `note-net-helper` and forward it to guest port 80.
    #[arg(long)]
    setup_address: bool,
    /// vmnet shared mode. The guest address would be learned from traffic.
    /// Without the entitlement this stays inactive and reports the vmnet status.
    /// Not a port forward. Do not combine with `--forward`, `--softap`, `--nat`,
    /// or `--setup-address`.
    #[arg(long)]
    shared: bool,
    /// Carry browser TCP to a SoftAP the guest is running, as the emulated station.
    /// `--softap` uses port 8080; `--softap 9090` picks another. With `--setup-address`
    /// the helper's `192.168.4.1:80` is the listener instead of loopback.
    #[arg(long, num_args = 0..=1, default_missing_value = "8080", value_name = "PORT")]
    softap: Option<u16>,
    /// WPA2-PSK passphrase for that SoftAP. Not printed.
    #[arg(long)]
    softap_psk: Option<String>,
    /// Read that passphrase from a mode-0600 file instead of the command line. Not printed.
    /// A missing file is polled until it appears, so it can be filled from the setup panel.
    #[arg(long)]
    softap_psk_file: Option<PathBuf>,
    /// After the SoftAP station takes a lease, take one software reset on the chip's own path.
    #[arg(long)]
    chip_reset_after_lease: bool,
    /// Helper control socket. Defaults to the installed per-user system socket when present,
    /// otherwise the development path under Application Support.
    #[arg(long)]
    helper_socket: Option<PathBuf>,
    /// Battery terminal voltage in mV.
    #[arg(long, default_value_t = 3900)]
    battery_mv: u32,
    /// Plug USB in at this virtual time: external supply on and the charger reports charging.
    #[arg(long)]
    usb_at: Option<f64>,
    /// Unplug USB at this virtual time: external supply off, charger idle, no USB host.
    #[arg(long)]
    usb_off_at: Option<f64>,
    /// `off`: no USB host on USB-Serial/JTAG from the start (a board on battery, no cable).
    /// The default `on` is a debug host attached, so the USB console works.
    #[arg(long, default_value = "on", value_parser = ["on", "off"])]
    usb_host: String,
    /// `host`: seed the chip's hardware RNG from host entropy on every boot (as radio noise does
    /// on silicon). The default `fixed` repeats the same `esp_random` sequence every boot.
    #[arg(long, default_value = "fixed", value_parser = ["fixed", "host"])]
    entropy: String,
    /// Use the interpreter only (the JIT's oracle; slower).
    #[arg(long)]
    no_jit: bool,
    /// Serve the GDB remote protocol on 127.0.0.1:PORT (`target remote :PORT`). Debug runs
    /// execute per instruction.
    #[arg(long)]
    gdb: Option<u16>,
    /// With --gdb: stay halted at the reset vector until GDB continues.
    #[arg(long)]
    gdb_wait: bool,
    /// Firmware ELF for backtrace symbolication, repeatable. An ESP-IDF build directory given
    /// as --firmware supplies its app and bootloader ELFs automatically.
    #[arg(long = "elf")]
    elves: Vec<PathBuf>,
    /// After the run, write the raw `coredump` partition from flash to this file (DEV-04);
    /// decode it with ESP-IDF's `esp-coredump --core-format raw`.
    #[arg(long)]
    coredump_out: Option<PathBuf>,
    /// Serve UART0 as an RFC 2217 port on 127.0.0.1:PORT for esptool / `idf.py -p
    /// rfc2217://127.0.0.1:PORT`; DTR/RTS drive EN and GPIO0 (download mode).
    #[arg(long)]
    serial_rfc2217: Option<u16>,
    /// Also offer the console as a pseudo-terminal at PATH (a symlink) for serial monitors;
    /// monitor only (no DTR/RTS on macOS PTYs). Carries `--serial-port`.
    #[arg(long, value_name = "PATH")]
    serial_pty: Option<PathBuf>,
    /// Which chip port `--serial-rfc2217` carries: `usb` (USB-Serial/JTAG, how NOTE boards are
    /// flashed) or `uart0`.
    #[arg(long, default_value = "usb")]
    serial_port: String,
    /// With `--avd`: boot from the quick-boot snapshot saved at the last graceful stop when the
    /// flash is unchanged since, and save a new one at stop. Any problem cold-boots (SNAP-05).
    #[arg(long)]
    quick_boot: bool,
    /// With `--quick-boot`: start from reset this time. The saved snapshot is discarded at once
    /// and a new one is saved at stop, so the next quick boot resumes this session.
    #[arg(long, requires = "quick_boot")]
    cold_boot: bool,
    /// With `--avd`: restore this named snapshot (`ndb -s AVD snapshot save NAME`) before running.
    #[arg(long, value_name = "NAME")]
    snapshot_load: Option<String>,
}

struct Press {
    at: u64,
    button: String,
    down: bool,
}

fn parse_presses(specs: &[String], profile: &Profile) -> Result<Vec<Press>, String> {
    let mut out = Vec::new();
    for spec in specs {
        let parts: Vec<&str> = spec.split(':').collect();
        let bad = || format!("--press {spec:?}: expected <seconds>:<button>[:<hold ms>]");
        if !(2..=3).contains(&parts.len()) {
            return Err(bad());
        }
        let at: f64 = parts[0].parse().map_err(|_| bad())?;
        let button = parts[1].to_string();
        if !profile.buttons.iter().any(|b| b.id == button) {
            return Err(format!(
                "--press {spec:?}: profile {} has no button {button:?}",
                profile.id
            ));
        }
        let hold_ms: f64 = parts
            .get(2)
            .map(|h| h.parse().map_err(|_| bad()))
            .transpose()?
            .unwrap_or(100.0);
        let down = (at * CPU_HZ as f64) as u64;
        out.push(Press {
            at: down,
            button: button.clone(),
            down: true,
        });
        out.push(Press {
            at: down + (hold_ms / 1000.0 * CPU_HZ as f64) as u64,
            button,
            down: false,
        });
    }
    out.sort_by_key(|p| p.at);
    Ok(out)
}

/// Line-oriented console presentation: the ROM and IDF print the same line on UART0 and the
/// USB-Serial/JTAG console. Collapse a line seen on the other channel within 0.5 s of virtual
/// time; repeats on the same channel stay (Spec §11.2). Raw captures are written separately.
struct ConsoleView {
    partial: [Vec<u8>; 2],
    recent: HashMap<Vec<u8>, (usize, u64)>,
    raw: bool,
    /// Resolves panic backtraces against the firmware ELF files (DEV-03).
    symbols: Option<note_runtime::symbolize::Symbolizer>,
}

impl ConsoleView {
    const WINDOW: u64 = CPU_HZ / 2;

    fn feed(&mut self, channel: usize, bytes: &[u8], now: u64, out: &mut impl Write) {
        self.partial[channel].extend_from_slice(bytes);
        while let Some(end) = self.partial[channel].iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.partial[channel].drain(..=end).collect();
            while matches!(line.last(), Some(b'\n' | b'\r')) {
                line.pop();
            }
            if !self.raw {
                if let Some(&(other, seen)) = self.recent.get(&line) {
                    if other != channel && now.saturating_sub(seen) <= Self::WINDOW {
                        self.recent.remove(&line);
                        continue;
                    }
                }
                self.recent.insert(line.clone(), (channel, now));
                if self.recent.len() > 4096 {
                    self.recent
                        .retain(|_, (_, t)| now.saturating_sub(*t) <= Self::WINDOW);
                }
            }
            let tag = if channel == 0 { "uart0" } else { "usb  " };
            let text = String::from_utf8_lossy(&line);
            let _ = writeln!(out, "[{tag}] {text}");
            if let Some(frames) = self.symbols.as_ref().and_then(|s| s.annotate(&text)) {
                for frame in frames {
                    let _ = writeln!(out, "[{tag}] {frame}");
                }
            }
        }
    }
}

/// The virtual AP spec: `--wifi`, or the SSID and passphrase from `--wifi-env`.
fn wifi_spec(args: &Args) -> Result<String, String> {
    let Some(path) = &args.wifi_env else { return Ok(args.wifi.clone()) };
    let meta = fs::metadata(path).map_err(|e| format!("--wifi-env {}: {e}", path.display()))?;
    if std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o077 != 0 {
        return Err(format!("--wifi-env {} must be mode 0600", path.display()));
    }
    let text = fs::read_to_string(path).map_err(|e| format!("--wifi-env {}: {e}", path.display()))?;
    wifi_spec_from_env(&text).map_err(|e| format!("--wifi-env {}: {e}", path.display()))
}

/// `WIFI_SSID` (required) and `WIFI_PASSWORD` (optional; empty is an open AP) from dotenv text.
/// Values may be quoted. The AP spec is comma-separated, so a comma in either value is refused.
fn wifi_spec_from_env(text: &str) -> Result<String, String> {
    let mut ssid = None;
    let mut psk = None;
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim();
        let value = value
            .strip_prefix('"').and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        match key.trim() {
            "WIFI_SSID" => ssid = Some(value.to_string()),
            "WIFI_PASSWORD" => psk = Some(value.to_string()),
            _ => {}
        }
    }
    let ssid = ssid.filter(|s| !s.is_empty()).ok_or("no WIFI_SSID")?;
    let psk = psk.filter(|p| !p.is_empty());
    if ssid.contains(',') || psk.as_deref().is_some_and(|p| p.contains(',')) {
        return Err("WIFI_SSID or WIFI_PASSWORD contains a comma, which the AP spec cannot carry".into());
    }
    if psk.as_deref().is_some_and(|p| !(8..=63).contains(&p.len())) {
        return Err("WIFI_PASSWORD must be 8 to 63 bytes for WPA2".into());
    }
    Ok(match psk {
        Some(psk) => format!("ssid={ssid},psk={psk}"),
        None => format!("ssid={ssid}"),
    })
}

/// Bind `--forward` / `--setup-address` and the status the protocol reports.
/// The headless run and `note-emu --avd` share this, so the device window shows
/// the listener that is actually open.
/// `Ok(None)` while the file is not there yet. The bytes are not logged.
fn read_softap_psk_file(path: &Path) -> Result<Option<String>, String> {
    let meta = match fs::metadata(path) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("softap passphrase file: {e}")),
        Ok(meta) => meta,
    };
    let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions());
    if mode & 0o077 != 0 {
        return Err("softap passphrase file must be mode 0600".into());
    }
    if meta.len() == 0 || meta.len() > 80 {
        return Err("softap passphrase file is empty or too long".into());
    }
    let text = fs::read_to_string(path).map_err(|e| format!("softap passphrase file: {e}"))?;
    let psk = text.trim();
    if !(8..=63).contains(&psk.len()) || psk.bytes().any(|b| b < 0x20) {
        return Err("softap passphrase file must hold one 8 to 63 byte line".into());
    }
    Ok(Some(psk.to_string()))
}

fn apply_softap_psk(nm: &mut NoteMachine, args: &Args) -> Result<bool, String> {
    if args.softap_psk.is_some() && args.softap_psk_file.is_some() {
        return Err("--softap-psk and --softap-psk-file are mutually exclusive".into());
    }
    if let Some(psk) = &args.softap_psk {
        nm.set_softap_passphrase(psk);
        return Ok(true);
    }
    if let Some(path) = &args.softap_psk_file {
        if let Some(psk) = read_softap_psk_file(path)? {
            nm.set_softap_passphrase(&psk);
            return Ok(true);
        }
    }
    Ok(false)
}

fn note_softap(nm: &NoteMachine, seen: &mut Option<[u8; 4]>, saw_privacy: &mut bool, phase: &mut &'static str) {
    let Some((ssid, privacy, has_psk, ip, now)) = nm.softap_sight() else { return };
    if now != *phase {
        eprintln!("[note-emu] softap {ssid} {now}");
        *phase = now;
    }
    if privacy && !has_psk && !*saw_privacy {
        eprintln!("[note-emu] softap {ssid} is protected; pass --softap-psk to join");
        *saw_privacy = true;
    }
    if let Some(ip) = ip {
        if *seen != Some(ip) {
            eprintln!("[note-emu] softap station {}.{}.{}.{} joined {ssid}", ip[0], ip[1], ip[2], ip[3]);
            eprintln!("[note-emu] flash fnv32 {:08x}", flash_fnv(nm.flash_contents()));
            *seen = Some(ip);
        }
    }
}

fn shared_permission(helper: &std::path::Path, args: &Args) -> String {
    if let Some(why) = incompatible_shared(args.softap.is_some(), args.setup_address, !args.forwards.is_empty(), args.nat) {
        return why.to_string();
    }
    let refused = request_shared(helper);
    if refused.code == "HelperUnavailable" {
        let local = probe_shared();
        let (gateway, mask) = match &local {
            SharedProbe::NoGuest { gateway, mask, .. } => (parse_ipv4(gateway), parse_ipv4(mask)),
            SharedProbe::Failed { .. } => (None, None),
        };
        let detail = shared_failure_permission(local.code(), &local.inactive_message(), gateway, mask);
        return format!("HelperUnavailable: {}; {detail}", refused.message);
    }
    shared_failure_permission(&refused.code, &refused.message, refused.gateway, refused.mask)
}

fn parse_ipv4(text: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = text.split('.');
    for slot in &mut out {
        *slot = parts.next()?.parse().ok()?;
    }
    if parts.next().is_some() { None } else { Some(out) }
}

fn flash_fnv(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c9dc5u32, |h, &b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

fn host_access(nm: &mut NoteMachine, args: &Args) -> Result<(NetworkInfo, Option<(Session, std::net::SocketAddr)>), String> {
    if args.shared {
        let helper = args.helper_socket.clone().unwrap_or_else(default_socket);
        let permission = shared_permission(&helper, args);
        let mut network = NetworkInfo::stopped(HostMode::Shared);
        network.deny_shared(&permission);
        eprintln!("[note-emu] shared mode inactive: {permission}");
        eprintln!("[note-emu] {}", network.access_scope);
        return Ok((network, None));
    }
    let session = if args.setup_address {
        let control = PathBuf::from(format!("/tmp/note-emu-{}/helper.sock", std::process::id()));
        let helper = args.helper_socket.clone().unwrap_or_else(default_socket);
        let mut lease = Session::acquire(&helper, &control).map_err(|e| {
            format!("setup address: {e} (is `note-net-helper run` up, and authorized?)")
        })?;
        let listener = lease
            .take_listener()
            .ok_or_else(|| "setup address: helper granted a lease without a socket".to_string())?;
        let bound = if args.softap.is_some() {
            nm.adopt_softap(listener, 80)
                .map_err(|e| format!("setup address: {e}"))?
        } else {
            nm.adopt_forward(listener, 80)
                .map_err(|e| format!("setup address: {e}"))?
        };
        let via = if args.softap.is_some() {
            "guest SoftAP"
        } else {
            "guest station"
        };
        eprintln!("[note-emu] setup address http://192.168.4.1/ -> {via} port 80 ({bound})");
        Some((lease, bound))
    } else {
        None
    };
    let mut bindings = Vec::new();
    let mut browser_url = None;
    if args.setup_address {
        browser_url = Some("http://192.168.4.1/".to_string());
        bindings.push("192.168.4.1:80".to_string());
    }
    for spec in &args.forwards {
        let forward = parse_forward(spec)?;
        let bound = match nm.listen_forward(forward.bind, forward.guest_port) {
            Ok(addr) => addr,
            Err(err) if err.kind() == ErrorKind::AddrInUse => {
                let mut fallback = forward.bind;
                fallback.set_port(0);
                let addr = nm
                    .listen_forward(fallback, forward.guest_port)
                    .map_err(|e| format!("--forward {spec}: {e}"))?;
                eprintln!("[note-emu] --forward {spec} is in use; using {addr}");
                addr
            }
            Err(err) => return Err(format!("--forward {spec}: {err}")),
        };
        eprintln!(
            "[note-emu] forwarding http://{bound}/ -> guest port {}",
            forward.guest_port
        );
        if browser_url.is_none() {
            browser_url = Some(format!("http://{bound}/"));
        }
        bindings.push(bound.to_string());
    }
    if let Some(port) = args.softap {
        if !args.setup_address {
            let mut bind = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port));
            let bound = match nm.listen_softap(bind, 80) {
                Ok(addr) => addr,
                Err(err) if err.kind() == ErrorKind::AddrInUse => {
                    bind.set_port(0);
                    let addr = nm
                        .listen_softap(bind, 80)
                        .map_err(|e| format!("--softap: {e}"))?;
                    eprintln!("[note-emu] --softap {port} is in use; using {addr}");
                    addr
                }
                Err(err) => return Err(format!("--softap: {err}")),
            };
            eprintln!("[note-emu] softap http://{bound}/ -> guest AP port 80 (after the station's DHCP lease)");
            if browser_url.is_none() {
                browser_url = Some(format!("http://{bound}/"));
            }
            bindings.push(bound.to_string());
        }
    }
    let mode = if args.setup_address {
        HostMode::Setup
    } else if args.nat || !args.forwards.is_empty() || args.softap.is_some() {
        HostMode::User
    } else {
        HostMode::Disabled
    };
    let mut network = NetworkInfo::stopped(mode);
    network.start(
        mode,
        browser_url,
        bindings,
        if args.setup_address {
            "authorized"
        } else {
            "not-required"
        },
    );
    eprintln!(
        "[note-emu] network {}, external access {}, discovery {}",
        network.active.as_str(),
        network.allows_external(),
        network.discovery
    );
    if let Some(url) = network.reported_url() {
        eprintln!("[note-emu] open {url}");
        eprintln!("[note-emu] {}", network.access_scope);
    }
    Ok((network, session))
}

fn write_png(path: &Path, profile: &Profile, visible: &[u8]) -> std::io::Result<()> {
    let d = &profile.display;
    let (w, h) = (d.width as usize, d.height as usize);
    let palette: Vec<[u8; 3]> = match d.format {
        DisplayFormat::Pal2 => d.palette.iter().map(|hex| parse_hex(hex)).collect(),
        DisplayFormat::Gray4 => (0..16u8).map(|l| [l * 17; 3]).collect(),
    };
    let mut rgb = Vec::with_capacity(w * h * 3);
    for i in 0..w * h {
        let index = match d.format {
            DisplayFormat::Pal2 => (visible[i / 4] >> (6 - 2 * (i % 4))) & 3,
            DisplayFormat::Gray4 => (visible[i / 2] >> if i % 2 == 0 { 4 } else { 0 }) & 0x0f,
        };
        rgb.extend_from_slice(&palette[index as usize]);
    }
    let file = fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(std::io::Error::other)?;
    writer.write_image_data(&rgb).map_err(std::io::Error::other)
}

fn parse_hex(hex: &str) -> [u8; 3] {
    let h = hex.trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    [byte(0), byte(2), byte(4)]
}

fn main() -> ExitCode {
    let args = Args::parse();
    let forwarder = if args.avd.is_some() { outlive_output_reader() } else { None };
    let code = match run(&args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("note-emu: {message}");
            ExitCode::from(2)
        }
    };
    if let Some(forwarder) = forwarder {
        // Close our ends so the forwarder sees EOF, then let it copy the last lines (a startup
        // error is what a launching app shows).
        // SAFETY: nothing writes to stdio after this point.
        unsafe {
            libc::close(1);
            libc::close(2);
        }
        let _ = forwarder.join();
    }
    code
}

/// A device must stop cleanly (commit flash, save its quick-boot snapshot) however its console
/// reader goes away: `eprintln!` panics on a closed pipe, so an emulator orphaned by its app used
/// to die at its next log line. stdout and stderr are moved onto an internal pipe that always has
/// a reader; a thread copies it to the original output and drops what can no longer be written.
fn outlive_output_reader() -> Option<std::thread::JoinHandle<()>> {
    let mut fds = [0i32; 2];
    // SAFETY: plain descriptor calls; on any failure the original stdio is left in place.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return None;
        }
        let original = libc::dup(2);
        if original < 0 || libc::dup2(fds[1], 1) < 0 || libc::dup2(fds[1], 2) < 0 {
            return None;
        }
        libc::close(fds[1]);
        let read_end = fds[0];
        Some(std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut open = true;
            loop {
                let n = libc::read(read_end, buf.as_mut_ptr().cast(), buf.len());
                if n < 0 && std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                    continue;
                }
                if n <= 0 {
                    break;
                }
                let mut sent = 0usize;
                while open && sent < n as usize {
                    let w = libc::write(original, buf[sent..].as_ptr().cast(), n as usize - sent);
                    if w > 0 {
                        sent += w as usize;
                    } else if w < 0 && std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                        continue;
                    } else {
                        open = false; // the reader is gone: keep draining, write nowhere
                    }
                }
            }
        }))
    }
}

fn cycles_to_ns(cycles: u64) -> u64 {
    ((cycles as u128) * 1_000_000_000 / CPU_HZ as u128) as u64
}

fn configure_mic(nm: &mut NoteMachine, args: &Args) -> Result<(), String> {
    let Some(path) = &args.mic_wav else { return Ok(()) };
    let wav = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    for i2s in [&mut nm.m.bus.periph.i2s0, &mut nm.m.bus.periph.i2s1] {
        i2s.push_rx_wav(&wav).map_err(|e| format!("{}: {e}", path.display()))?;
        i2s.loopback = false;
        i2s.rx_idle_silence = false;
    }
    Ok(())
}

fn save_capture(args: &Args, capture: &mut FrameCapture, now_ns: u64, nm: &NoteMachine) -> Result<(), String> {
    let Some(dir) = &args.out else { return Ok(()) };
    if args.capture_gif && capture.frame_count() > 0 {
        capture.finish(now_ns);
        let path = dir.join("panel.gif");
        fs::write(&path, capture.gif().map_err(|e| e.to_string())?).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    if args.capture_wav {
        let audio = nm.m.bus.periph.audio();
        let path = dir.join("speaker.wav");
        fs::write(&path, esp32s3::periph::I2s::wav_from_pcm(&audio.pcm, audio.sample_rate))
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Boot a stored AVD on the chip and serve the protocol socket until virtual time
/// runs out. The busy lock is held for the whole run; flash is committed on the way out.
/// Restore the quick-boot snapshot if it belongs to exactly this flash. A snapshot that fails
/// to apply is removed so the next start does not try it again; the machine is unchanged then.
fn quick_boot(nm: &mut NoteMachine, dir: &Path, flash_sha256: &str, firmware: [u8; 32]) -> Result<(), String> {
    let snap = dir.join("quickboot.snap");
    let paired = dir.join("quickboot.flash-sha256");
    let bytes = fs::read(&snap).map_err(|_| "no quick-boot snapshot".to_string())?;
    let expect = fs::read_to_string(&paired).map_err(|_| "quick-boot snapshot has no flash pairing".to_string())?;
    if expect.trim() != flash_sha256 {
        return Err("flash changed since the quick-boot snapshot".into());
    }
    nm.restore_snapshot(&bytes, firmware).map(|_| ()).map_err(|e| {
        let _ = fs::remove_file(&snap);
        let _ = fs::remove_file(&paired);
        format!("quick-boot snapshot rejected ({e}); removed")
    })
}

/// The AVD's base firmware hash (hex SHA-256 in its config) as snapshot identity.
fn firmware_identity(hex: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = hex.get(i * 2..i * 2 + 2).and_then(|h| u8::from_str_radix(h, 16).ok()).unwrap_or(0);
    }
    out
}

/// SIGTERM / SIGINT ask for a graceful stop: the AVD loop ends as if `stop` had been
/// requested, so flash is committed and a quick-boot snapshot saved.
static STOP_SIGNAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_stop_signal(_: libc::c_int) {
    STOP_SIGNAL.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn install_stop_signals() {
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGTERM, on_stop_signal as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_stop_signal as libc::sighandler_t);
    }
}

/// The AVD's saved network mode, unless the command line chose one. NOTE4C firmware provisions
/// over its own SoftAP, so its browser path is the emulated station (`--softap`) with NAT.
fn with_avd_network(args: &Args, mode: Option<&str>, profile: &str) -> Result<Args, String> {
    let mut args = args.clone();
    let explicit = args.nat || !args.forwards.is_empty() || args.setup_address || args.shared || args.softap.is_some();
    if explicit {
        return Ok(args);
    }
    let softap = profile == "note4c";
    match mode.unwrap_or("disabled") {
        "disabled" => {}
        "user" => {
            args.nat = true;
            if softap { args.softap = Some(8080); }
        }
        "setup" => {
            args.setup_address = true;
            if softap { args.softap = Some(8080); args.nat = true; }
        }
        "shared" => args.shared = true,
        other => return Err(format!("AVD network mode {other:?} is not one of disabled, user, setup, shared")),
    }
    Ok(args)
}

/// Real-time pacing: keep virtual time level with the wall clock from a baseline. A quick boot or
/// `snapshot.load` moves the guest clock, so a jump re-bases instead of sleeping (or racing) the
/// difference away. Sleeps are short so the control socket keeps being served.
struct Pacer {
    virtual_ns: u64,
    last_ns: u64,
    wall: Instant,
}

impl Pacer {
    const MAX_SLEEP: Duration = Duration::from_millis(20);
    /// Farther ahead than a few slices can only be a restored clock.
    const JUMP: Duration = Duration::from_secs(1);

    fn new(virtual_ns: u64) -> Pacer {
        Pacer { virtual_ns, last_ns: virtual_ns, wall: Instant::now() }
    }

    /// How long to wait before running past `now_ns`, re-basing on a clock jump.
    fn ahead(&mut self, now_ns: u64, elapsed: Duration) -> Option<Duration> {
        if now_ns < self.last_ns {
            *self = Self::new(now_ns);
            return None;
        }
        self.last_ns = now_ns;
        let run = Duration::from_nanos(now_ns - self.virtual_ns);
        if run > elapsed + Self::JUMP {
            self.virtual_ns = now_ns;
            self.wall = Instant::now();
            return None;
        }
        run.checked_sub(elapsed).map(|d| d.min(Self::MAX_SLEEP))
    }

    fn pace(&mut self, now_ns: u64) {
        let elapsed = self.wall.elapsed();
        if let Some(wait) = self.ahead(now_ns, elapsed) {
            std::thread::sleep(wait);
        }
    }
}

fn run_avd(args: &Args, id: &str) -> Result<ExitCode, String> {
    install_stop_signals();
    let store = Store::new(paths::data_home());
    let held = store.lock(id).map_err(|e| e.to_string())?;
    let config = store.config(id).map_err(|e| e.to_string())?;
    let mut args = with_avd_network(args, config.network.as_deref(), &config.profile)?;
    if args.wifi_env.is_none() {
        args.wifi_env = config.wifi_env.as_ref().map(PathBuf::from);
    }
    let args = &args;
    let profile =
        profile::find(&profile::profiles_dir(), &config.profile).map_err(|e| e.to_string())?;
    let rom_info = rom::installed().map_err(|e| e.to_string())?;
    let rom_elf = fs::read(&rom_info.path).map_err(|e| format!("read ROM: {e}"))?;
    let bytes = store.flash(id).map_err(|e| e.to_string())?;
    let flash = FlashImage {
        layout: Layout::MergedImage,
        bytes,
        segments: Vec::new(),
    };
    let mut presses = parse_presses(&args.presses, &profile)?
        .into_iter()
        .peekable();
    if let Some(out) = &args.out {
        fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    }
    let rtc_base = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let mac = [0x02, 0x4e, 0x4f, 0x54, 0x45, if profile.id == "note4" { 0x04 } else { 0x4c }];
    let mut nm = NoteMachine::new(&profile, &rom_elf, &flash, mac, rtc_base, &RadioConfig { ap: wifi_spec(args)?, nat: args.nat })?;
    if args.chip_reset_after_lease {
        return Err("--chip-reset-after-lease applies to a headless run, not --avd".into());
    }
    let loaded = apply_softap_psk(&mut nm, args)?;
    if args.softap_psk_file.is_some() && !loaded {
        return Err("softap passphrase file is missing".into());
    }
    configure_mic(&mut nm, args)?;
    nm.set_battery_mv(args.battery_mv);
    nm.set_usb_host(args.usb_host == "on");
    nm.set_host_entropy(args.entropy == "host");
    if args.no_jit {
        nm.set_jit(false);
    }
    let (network, mut helper) = host_access(&mut nm, args)?;
    let mut next_heartbeat = Instant::now();
    let instance_id = format!("{:08x}{:08x}", std::process::id(), rtc_base as u32);
    let control = PathBuf::from(format!("/tmp/note-emu-{}/control.sock", std::process::id()));
    eprintln!(
        "[note-emu] AVD {id} ({}) instance {instance_id}",
        profile.id
    );
    eprintln!("[note-emu] control {}", control.display());

    nm.set_firmware_hash(firmware_identity(&config.sha256));
    let snapshots = store.snapshot_dir(id).map_err(|e| e.to_string())?;
    let flash_hash = |bytes: &[u8]| -> String {
        use sha2::Digest;
        sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    };
    if args.quick_boot && args.cold_boot {
        let _ = fs::remove_file(snapshots.join("quickboot.snap"));
        let _ = fs::remove_file(snapshots.join("quickboot.flash-sha256"));
        eprintln!("[note-emu] cold boot (requested); state is saved at stop");
    } else if args.quick_boot && args.snapshot_load.is_none() {
        let current = flash_hash(nm.flash_contents());
        match quick_boot(&mut nm, &snapshots, &current, firmware_identity(&config.sha256)) {
            Ok(()) => eprintln!("[note-emu] quick boot at t={:.3}s", nm.seconds()),
            Err(reason) => eprintln!("[note-emu] cold boot: {reason}"),
        }
    }
    if let Some(name) = &args.snapshot_load {
        let bytes = fs::read(snapshots.join(format!("{name}.snap"))).map_err(|e| format!("snapshot {name}: {e}"))?;
        let report = nm.restore_snapshot(&bytes, firmware_identity(&config.sha256)).map_err(|e| format!("snapshot {name}: {e}"))?;
        eprintln!("[note-emu] restored snapshot {name} at t={:.3}s (released {:?}, reconnect {})",
                  nm.seconds(), report.released, report.reconnect_pending);
    }
    // Developer endpoints, as in a headless run (DEV-01/02 on a stored AVD).
    let mut serial = match args.serial_rfc2217 {
        Some(port) => {
            let which = if args.serial_port == "uart0" { serialserve::Port::Uart0 } else { serialserve::Port::UsbSerialJtag };
            Some(serialserve::SerialServe::start(port, which)?)
        }
        None => None,
    };
    let mut gdb = match args.gdb {
        Some(port) => Some(gdbserve::GdbServe::start(&mut nm, port, args.gdb_wait)?),
        None => None,
    };
    let mut pty = match &args.serial_pty {
        Some(path) => Some(serialpty::SerialPty::start(path, args.serial_port != "uart0")?),
        None => None,
    };
    let mut inst = Instance::new(nm, 16 * 1024 * 1024, 64);
    inst.set_identity(id, &instance_id);
    inst.set_dev_ports(args.serial_rfc2217, args.gdb);
    if args.serial_pty.is_some() && args.serial_rfc2217.is_none() {
        inst.tap_console();
    }
    inst.set_snapshot_dir(snapshots.clone());
    inst.set_network(network);
    let mut host = OwnedServer::bind(inst, &control).map_err(|e| format!("control socket: {e}"))?;
    // Publish only once the socket accepts connections: `ndb -s`, the app and scripts treat a
    // published instance as reachable (a quick-boot restore can take seconds before this).
    let published = store
        .publish_run(&instance_id, id, &control)
        .map_err(|e| e.to_string())?;
    let _unpublish = scopeguard_file(published);
    let viewer = host.inst.attach();
    // `0` is the interactive session the app opens: virtual time does not end it.
    let unlimited = args.seconds <= 0.0;
    let end_ns = if unlimited {
        u64::MAX
    } else {
        (args.seconds * 1_000_000_000.0) as u64
    };
    let slice_ns = 10_000_000u64;
    let started = Instant::now();
    let mut pacer = Pacer::new(host.inst.guest.now_ns());
    let mut pacing_revision = host.inst.pacing_revision();
    let mut seen_version = 0u64;
    let mut frame_no = 0u64;
    let mut capture = FrameCapture::from_display(&profile.display);
    let mut seen_softap = None;
    let mut saw_privacy = false;
    let mut softap_phase = "";
    let mut exit = ExitCode::SUCCESS;
    loop {
        host.poll().map_err(|e| format!("control: {e}"))?;
        if pacing_revision != host.inst.pacing_revision() {
            pacing_revision = host.inst.pacing_revision();
            pacer = Pacer::new(host.inst.guest.now_ns());
        }
        note_softap(
            &host.inst.guest,
            &mut seen_softap,
            &mut saw_privacy,
            &mut softap_phase,
        );
        if let Some((lease, bound)) = helper.as_mut() {
            let _ = lease.poll_control();
            if Instant::now() >= next_heartbeat {
                next_heartbeat = Instant::now() + Duration::from_secs(2);
                if lease.holding() && lease.heartbeat().is_err() {
                    eprintln!("[note-emu] helper disconnected; closing the setup listener");
                    host.inst.guest.close_forward(*bound);
                }
            }
        }
        if !host.inst.is_running() || STOP_SIGNAL.load(std::sync::atomic::Ordering::SeqCst) {
            eprintln!("[note-emu] stop");
            break;
        }
        if let Some(g) = gdb.as_mut() {
            g.service(&mut host.inst.guest, Duration::from_millis(20));
            if g.kill {
                break;
            }
            if g.halted {
                pacer = Pacer::new(host.inst.guest.now_ns());
                continue;
            }
        }
        if let Some(p) = pty.as_mut() {
            p.poll(&mut host.inst.guest);
        }
        if let Some(s) = serial.as_mut() {
            s.poll(&mut host.inst.guest);
            if s.in_reset {
                pacer = Pacer::new(host.inst.guest.now_ns());
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
        }
        // A paused guest does not advance, which would otherwise look like a stall
        // and tear the session down. Keep serving the socket.
        if host.inst.guest.paused() {
            pacer = Pacer::new(host.inst.guest.now_ns());
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let (now_ns, stop_run, version, visible) = {
            let inst = &mut host.inst;
            let now = inst.guest.now_ns();
            if now >= end_ns {
                break;
            }
            let mut target = (now + slice_ns).min(end_ns);
            if let Some(press) = presses.peek() {
                let at = cycles_to_ns(press.at);
                if at > now {
                    target = target.min(at);
                }
            }
            while presses.peek().is_some_and(|p| cycles_to_ns(p.at) <= target) {
                let press = presses.next().unwrap();
                let at = cycles_to_ns(press.at);
                if at > inst.guest.now_ns() {
                    inst.advance(at);
                }
                inst.guest
                    .button(&press.button, press.down)
                    .map_err(|e| e.to_string())?;
                eprintln!(
                    "[note-emu] t={:.3}s button {} {}",
                    inst.guest.now_ns() as f64 / 1e9,
                    press.button,
                    if press.down { "down" } else { "up" }
                );
            }
            let before = inst.guest.now_ns();
            inst.advance(target);
            let debug_halt = gdb.as_mut().map(|g| { g.poll_stop(); g.halted }).unwrap_or(false);
            if serial.is_some() || pty.is_some() {
                let [uart0, usb] = inst.take_tapped();
                let console = note_machine::Console { uart0, usb };
                if let Some(s) = serial.as_mut() { s.send(&console); }
                if let Some(p) = pty.as_mut() { p.send(&console); }
            }
            // A press scheduled exactly at the slice end leaves nothing to advance; only a
            // slice that had time to run and did not is a stall (a breakpoint is not one).
            if inst.guest.now_ns() == before && before < target && !debug_halt {
                eprintln!("[note-emu] t={:.3}s execution stopped", before as f64 / 1e9);
                exit = ExitCode::from(1);
                (before, true, 0, None)
            } else {
                for item in inst.drain(viewer) {
                    match item {
                        Queued::Line { channel, text, .. } => {
                            let tag = if channel == 0 { "uart0" } else { "usb" };
                            eprintln!("[{tag}] {text}");
                        }
                        Queued::Gap { dropped } => {
                            eprintln!("[note-emu] log gap, dropped {dropped} lines")
                        }
                    }
                }
                let version = inst.guest.display_version();
                let visible = (version != seen_version).then(|| inst.guest.frame().to_vec());
                (inst.guest.now_ns(), false, version, visible)
            }
        };
        if stop_run {
            break;
        }
        if let Some(pixels) = visible {
            seen_version = version;
            frame_no += 1;
            if args.capture_gif { capture.commit(&pixels, now_ns); }
            eprintln!(
                "[note-emu] t={:.3}s frame {frame_no} (wall {:.2}s)",
                now_ns as f64 / 1e9,
                started.elapsed().as_secs_f64()
            );
            if let Some(dir) = &args.out {
                let path = dir.join(format!("frame-{frame_no:03}.png"));
                write_png(&path, &profile, &pixels)
                    .map_err(|e| format!("write {}: {e}", path.display()))?;
            }
        }
        if args.frames.is_some_and(|n| frame_no >= n) {
            break;
        }
        if args.realtime {
            pacer.pace(now_ns);
        }
    }
    save_capture(args, &mut capture, host.inst.guest.now_ns(), &host.inst.guest)?;
    if let Some((lease, bound)) = helper {
        if lease.holding() {
            host.inst.guest.close_forward(bound);
            if let Err(err) = lease.release() {
                eprintln!("[note-emu] setup address release: {err}");
            }
        }
    }
    let flash_now = host.inst.guest.flash_contents().to_vec();
    store.commit(id, &flash_now).map_err(|e| e.to_string())?;
    if args.quick_boot {
        // Snapshot and flash are taken at the same boundary; the hash pairs them.
        let saved = host.inst.guest.save_snapshot(firmware_identity(&config.sha256)).map_err(|e| e.to_string())
            .and_then(|bytes| fs::create_dir_all(&snapshots).map(|()| bytes).map_err(|e| e.to_string()))
            .and_then(|bytes| note_machine::snapshot::publish(&snapshots.join("quickboot.snap"), &bytes).map_err(|e| e.to_string()))
            .and_then(|()| fs::write(snapshots.join("quickboot.flash-sha256"), flash_hash(&flash_now)).map_err(|e| e.to_string()));
        match saved {
            Ok(()) => eprintln!("[note-emu] quick-boot snapshot saved"),
            Err(err) => {
                let _ = fs::remove_file(snapshots.join("quickboot.flash-sha256"));
                eprintln!("[note-emu] quick-boot snapshot not saved: {err}");
            }
        }
    }
    drop(held);
    let _ = _unpublish;
    eprintln!(
        "[note-emu] AVD {id} stopped, flash committed, {} frames",
        frame_no
    );
    Ok(exit)
}

struct RemoveOnDrop(PathBuf);
impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        if let Some(parent) = self.0.parent() {
            let _ = fs::remove_dir(parent);
        }
    }
}
fn scopeguard_file(path: PathBuf) -> RemoveOnDrop {
    RemoveOnDrop(path)
}

fn run(args: &Args) -> Result<ExitCode, String> {
    if let Some(id) = &args.avd {
        return run_avd(args, id);
    }
    let profile_id = args
        .profile
        .as_deref()
        .ok_or("--profile is required without --avd")?;
    let firmware = args
        .firmware
        .as_ref()
        .ok_or("--firmware is required without --avd")?;
    let profile = profile::find(&profile::profiles_dir(), profile_id).map_err(|e| e.to_string())?;
    let rom_info = rom::installed().map_err(|e| e.to_string())?;
    let rom_elf = fs::read(&rom_info.path).map_err(|e| format!("read ROM: {e}"))?;
    let flash = note_core::flash::load(firmware, profile.flash_mib as usize * 1024 * 1024)
        .map_err(|e| e.to_string())?;
    let mut presses = parse_presses(&args.presses, &profile)?
        .into_iter()
        .peekable();
    if let Some(out) = &args.out {
        fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    }
    let mut raw_files = match &args.out {
        Some(out) => Some([
            fs::File::create(out.join("uart0.raw")).map_err(|e| e.to_string())?,
            fs::File::create(out.join("usb.raw")).map_err(|e| e.to_string())?,
        ]),
        None => None,
    };

    let rtc_base = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mac = [
        0x02,
        0x4e,
        0x4f,
        0x54,
        0x45,
        if profile.id == "note4" { 0x04 } else { 0x4c },
    ];
    let radio = RadioConfig {
        ap: wifi_spec(args)?,
        nat: args.nat,
    };
    let mut nm = NoteMachine::new(&profile, &rom_elf, &flash, mac, rtc_base, &radio)?;
    let mut psk_loaded = apply_softap_psk(&mut nm, args)?;
    configure_mic(&mut nm, args)?;
    nm.set_battery_mv(args.battery_mv);
    nm.set_usb_host(args.usb_host == "on");
    nm.set_host_entropy(args.entropy == "host");
    if args.no_jit {
        nm.set_jit(false);
    }
    if args.log_periph {
        use note_machine::SocBusExt;
        nm.m.bus.set_log_unknown(true);
    }
    eprintln!(
        "[note-emu] {} ({}), firmware {} ({:?}, {} segments), ROM {}",
        profile.name,
        profile.id,
        firmware.display(),
        flash.layout,
        flash.segments.len(),
        rom_info.release
    );

    let (_network, mut session) = host_access(&mut nm, args)?;
    let mut next_heartbeat = Instant::now();
    let mut serial = match args.serial_rfc2217 {
        Some(port) => {
            let which = match args.serial_port.as_str() {
                "usb" => serialserve::Port::UsbSerialJtag,
                "uart0" => serialserve::Port::Uart0,
                other => return Err(format!("--serial-port {other:?}: expected usb or uart0")),
            };
            Some(serialserve::SerialServe::start(port, which)?)
        }
        None => None,
    };
    let mut gdb = match args.gdb {
        Some(port) => Some(gdbserve::GdbServe::start(&mut nm, port, args.gdb_wait)?),
        None => None,
    };
    let mut pty = match &args.serial_pty {
        Some(path) => Some(serialpty::SerialPty::start(path, args.serial_port != "uart0")?),
        None => None,
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let symbols = symbolizer_for(args)?;
    let mut view = ConsoleView {
        symbols,
        partial: [Vec::new(), Vec::new()],
        recent: HashMap::new(),
        raw: args.raw_console,
    };
    let slice = CPU_HZ / 100; // 10 ms of virtual time
    let end = if args.seconds <= 0.0 { u64::MAX } else { (args.seconds * CPU_HZ as f64) as u64 };
    let started = Instant::now();
    let mut pacer = Pacer::new((nm.seconds() * 1e9) as u64);
    let mut seen_version = 0u64;
    let mut seen_refreshes = 0usize;
    let mut frame_no = 0u64;
    let mut capture = FrameCapture::from_display(&profile.display);
    let mut seen_softap = None;
    let mut saw_privacy = false;
    let mut softap_phase = "";
    let mut reset_requested = false;
    let mut exit = ExitCode::SUCCESS;
    let mut usb_events: Vec<(u64, bool)> = [(args.usb_at, true), (args.usb_off_at, false)]
        .into_iter()
        .filter_map(|(at, on)| at.map(|s| ((s * CPU_HZ as f64) as u64, on)))
        .collect();
    usb_events.sort();

    while nm.cycles() < end {
        if let Some(g) = gdb.as_mut() {
            g.service(&mut nm, Duration::from_millis(20));
            if g.kill {
                break;
            }
            if g.halted {
                pacer = Pacer::new((nm.seconds() * 1e9) as u64);
                continue;
            }
        }
        if let Some(p) = pty.as_mut() {
            p.poll(&mut nm);
        }
        if let Some(s) = serial.as_mut() {
            s.poll(&mut nm);
            if s.in_reset {
                pacer = Pacer::new((nm.seconds() * 1e9) as u64);
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
        }
        note_softap(&nm, &mut seen_softap, &mut saw_privacy, &mut softap_phase);
        if !psk_loaded {
            if let Some(path) = &args.softap_psk_file {
                if let Some(psk) = read_softap_psk_file(path)? {
                    nm.set_softap_passphrase(&psk);
                    psk_loaded = true;
                    eprintln!("[note-emu] softap passphrase loaded");
                }
            }
        }
        if args.chip_reset_after_lease && !reset_requested && seen_softap.is_some() {
            nm.request_reset();
            reset_requested = true;
            eprintln!("[note-emu] chip reset requested after the softap lease");
        }
        let mut target = (nm.cycles() + slice).min(end);
        if let Some(p) = presses.peek() {
            target = target.min(p.at.max(nm.cycles() + 1));
        }
        for at in usb_events.iter().map(|(at, _)| *at) {
            if at > nm.cycles() {
                target = target.min(at);
            }
        }
        let result = nm.run_until(target);
        while let Some(&(at, plugged)) = usb_events.first() {
            if at > nm.cycles() {
                break;
            }
            usb_events.remove(0);
            nm.set_external_power(plugged);
            if plugged { nm.set_usb_host(true); } else { nm.set_usb_host(false); }
            let sense = nm.set_charger(plugged, None);
            eprintln!("[note-emu] t={:.3}s usb {} (charger {sense:?})", nm.seconds(), if plugged { "plugged" } else { "unplugged" });
        }
        while presses.peek().is_some_and(|p| p.at <= nm.cycles()) {
            let p = presses.next().unwrap();
            nm.set_button(&p.button, p.down)?;
            eprintln!(
                "[note-emu] t={:.3}s button {} {}",
                nm.seconds(),
                p.button,
                if p.down { "down" } else { "up" }
            );
        }
        let console = nm.take_console();
        if let Some(s) = serial.as_mut() {
            s.send(&console);
        }
        if let Some(p) = pty.as_mut() {
            p.send(&console);
        }
        let now = nm.cycles();
        for (channel, bytes) in [(0, &console.uart0), (1, &console.usb)] {
            if let Some(files) = &mut raw_files {
                let _ = files[channel].write_all(bytes);
            }
            view.feed(channel, bytes, now, &mut out);
        }
        {
            let st = nm.board.lock();
            for r in &st.panel.refreshes[seen_refreshes..] {
                eprintln!(
                    "[note-emu] t={:.3}s panel {:?} refresh rect={:?} (wall {:.2}s)",
                    r.cycle as f64 / CPU_HZ as f64,
                    r.kind,
                    r.rect,
                    started.elapsed().as_secs_f64()
                );
            }
            seen_refreshes = st.panel.refreshes.len();
        }
        // The panel, or a legacy build's console frame (`display_source` 2).
        if nm.display_version() != seen_version {
            seen_version = nm.display_version();
            frame_no += 1;
            let pixels = nm.display_frame();
            if nm.display_source() == 2 {
                eprintln!("[note-emu] t={:.3}s legacy console frame", nm.seconds());
            }
            if args.capture_gif { capture.commit(&pixels, cycles_to_ns(nm.cycles())); }
            if let Some(dir) = &args.out {
                let path = dir.join(format!("frame-{frame_no:03}.png"));
                write_png(&path, &profile, &pixels)
                    .map_err(|e| format!("write {}: {e}", path.display()))?;
                eprintln!("[note-emu] wrote {}", path.display());
            }
        }
        match result {
            SliceEnd::Reached => {}
            SliceEnd::DebugStop => {
                if let Some(g) = gdb.as_mut() {
                    g.on_debug_stop();
                }
            }
            SliceEnd::Rebooted { cause, name } => {
                eprintln!("[note-emu] t={:.3}s chip reset, cause {cause:#x} ({name})", nm.seconds());
                eprintln!("[note-emu] flash fnv32 {:08x}", flash_fnv(nm.flash_contents()));
                seen_softap = None;
                softap_phase = "";
            }
            SliceEnd::Stopped(why) => {
                eprintln!("[note-emu] t={:.3}s execution stopped: {why}", nm.seconds());
                exit = ExitCode::from(1);
                break;
            }
            SliceEnd::PoweredOff => {
                eprintln!(
                    "[note-emu] t={:.3}s battery latch open; board power off (not a reset)",
                    nm.seconds()
                );
                break;
            }
        }
        if args.frames.is_some_and(|n| frame_no >= n) {
            break;
        }
        if args.realtime {
            pacer.pace((nm.seconds() * 1e9) as u64);
        }
        if let Some((lease, bound)) = session.as_mut() {
            let _ = lease.poll_control();
            if Instant::now() >= next_heartbeat {
                next_heartbeat = Instant::now() + Duration::from_secs(2);
                if lease.holding() && lease.heartbeat().is_err() {
                    eprintln!("[note-emu] helper disconnected; closing the setup listener");
                    nm.close_forward(*bound);
                }
            }
        }
    }
    save_capture(args, &mut capture, cycles_to_ns(nm.cycles()), &nm)?;
    if let Some(path) = &args.coredump_out {
        match note_core::flash::coredump(nm.flash_contents()) {
            Some(dump) => {
                std::fs::write(path, dump).map_err(|e| format!("write {}: {e}", path.display()))?;
                eprintln!("[note-emu] core dump partition written to {} ({} bytes)", path.display(), dump.len());
            }
            None => eprintln!("[note-emu] no core dump in flash (no coredump partition, or it is erased)"),
        }
    }
    if let Some((lease, bound)) = session {
        if lease.holding() {
            nm.close_forward(bound);
            if let Err(err) = lease.release() {
                eprintln!("[note-emu] setup address release: {err}");
            }
        }
    }

    let wall = started.elapsed().as_secs_f64();
    if args.dump {
        eprintln!("{}", nm.m.dump_regs());
        let pc = nm.m.cores[0].pc;
        eprintln!(
            "[note-emu] core0 code around {pc:#010x}:\n{}",
            nm.m.disasm(pc.wrapping_sub(0x30), 28)
        );
        if let Ok(extra) = std::env::var("NOTE_EMU_DISASM") {
            for spec in extra.split(',') {
                if let Some(word) = spec.strip_prefix("peek:") {
                    if let Ok(addr) = u32::from_str_radix(word.trim_start_matches("0x"), 16) {
                        eprintln!("[note-emu] peek {addr:#010x}:\n{}", nm.m.peek(addr, 4));
                    }
                } else if let Ok(addr) = u32::from_str_radix(spec.trim_start_matches("0x"), 16) {
                    eprintln!(
                        "[note-emu] code at {addr:#010x}:\n{}",
                        nm.m.disasm(addr, 40)
                    );
                }
            }
        }
        eprintln!("{}", esp_soc_report(&nm));
    }
    let st = nm.board.lock();
    eprintln!("[note-emu] stop at t={:.3}s virtual, {:.1}s wall ({:.1}x real time), {} insns ({:.1} Minsn/s), {} reboots, {} frames",
              nm.seconds(), wall, nm.seconds() / wall, nm.m.insns(), nm.m.insns() as f64 / wall / 1e6, nm.reboots, frame_no);
    eprintln!("[note-emu] rtc: {} sleeps (light or deep)", nm.m.bus.periph.rtc.sleeps);
    eprintln!(
        "[note-emu] panel: {} refreshes, {} temperature readbacks; {} battery ADC conversions",
        st.panel.refreshes.len(),
        st.panel.readbacks,
        st.adc_conversions
    );
    let mut counts: HashMap<String, usize> = HashMap::new();
    for d in &st.panel.diagnostics {
        *counts.entry(format!("{d:?}")).or_default() += 1;
    }
    for (d, n) in counts {
        eprintln!("[note-emu] panel diagnostic ×{n}: {d}");
    }
    Ok(exit)
}

#[cfg(test)]
mod shared_mode {
    use std::path::Path;

    use clap::Parser;
    use note_runtime::{HostMode, NetworkInfo};

    use super::{shared_permission, Args};

    fn args(extra: &[&str]) -> Args {
        let mut cmd = vec!["note-emu", "--profile", "note4", "--firmware", "unused"];
        cmd.extend_from_slice(extra);
        Args::try_parse_from(cmd).expect("parse")
    }

    #[test]
    fn shared_with_a_forward_is_rejected_and_does_not_call_vmnet() {
        let permission = shared_permission(Path::new("/tmp/note-shared-should-not-be-contacted.sock"), &args(&["--shared", "--forward", "9"]));
        assert!(permission.contains("per-service"), "{permission}");
        assert!(!permission.contains("VMNET_"), "{permission}");
        let mut info = NetworkInfo::stopped(HostMode::Shared);
        info.deny_shared(&permission);
        assert_eq!(info.active, HostMode::Disabled);
        assert!(info.reported_url().is_none());
        assert!(info.guest_addresses.is_empty());
    }

    /// No helper is listening. The status is from `vmnet_start_interface` in
    /// this process, not from a stand-in network.
    #[test]
    fn selecting_shared_without_a_helper_reports_the_live_vmnet_failure() {
        let permission = shared_permission(Path::new("/tmp/note-net-helper-not-running.sock"), &args(&["--shared"]));
        assert!(permission.starts_with("HelperUnavailable:"), "{permission}");
        assert!(permission.contains("VMNET_FAILURE (1001)"), "{permission}");
        assert!(!permission.contains("192.168.2.2"), "{permission}");
        let mut info = NetworkInfo::stopped(HostMode::User);
        info.start(HostMode::User, Some("http://127.0.0.1:9/".into()), vec!["127.0.0.1:9".into()], "not-required");
        info.deny_shared(&permission);
        assert_eq!(info.configured, HostMode::Shared);
        assert_eq!(info.active, HostMode::Disabled);
        assert!(!info.running);
        assert!(!info.reachable);
        assert!(info.reported_url().is_none());
        assert!(info.guest_addresses.is_empty());
        assert!(info.bindings.is_empty());
        assert!(info.report()["browser_url"].is_null());
    }
}

fn esp_soc_report(nm: &NoteMachine) -> String {
    let st = nm.board.lock();
    let pins: Vec<String> = st
        .outputs
        .iter()
        .enumerate()
        .filter_map(|(i, l)| l.map(|l| format!("gpio{i}={}", l as u8)))
        .collect();
    format!(
        "[note-emu] board outputs: {}; SPI bytes {}; refreshes {}; battery ADC conversions {}",
        pins.join(" "),
        st.spi_bytes,
        st.panel.refreshes.len(),
        st.adc_conversions
    )
}

/// ELF files for symbolication: `--elf` paths, plus the top-level and bootloader ELFs of an
/// ESP-IDF build directory given as `--firmware`.
fn symbolizer_for(args: &Args) -> Result<Option<note_runtime::symbolize::Symbolizer>, String> {
    let mut elves = args.elves.clone();
    if let Some(fw) = args.firmware.as_ref().filter(|p| p.is_dir()) {
        for dir in [fw.clone(), fw.join("bootloader")] {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut found: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|x| x == "elf")).collect();
                found.sort();
                elves.extend(found);
            }
        }
    }
    if elves.is_empty() {
        return Ok(None);
    }
    let s = note_runtime::symbolize::Symbolizer::open(&elves)?;
    eprintln!("[note-emu] symbols from {} ELF file(s)", elves.len());
    Ok(Some(s))
}

#[cfg(test)]
mod avd_network_tests {
    use super::*;

    fn parse(extra: &[&str]) -> Args {
        Args::parse_from(["note-emu", "--avd", "x"].iter().chain(extra))
    }

    #[test]
    fn pacing_rebases_on_a_restored_clock_and_never_sleeps_long() {
        // Quick boot: the guest clock starts minutes in; no multi-minute sleep.
        let mut p = Pacer::new(0);
        assert_eq!(p.ahead(240_000_000_000, Duration::ZERO), None);
        assert_eq!(p.virtual_ns, 240_000_000_000);
        // Normal run slightly ahead of the wall: a short wait, capped.
        assert_eq!(p.ahead(240_005_000_000, Duration::from_millis(2)), Some(Duration::from_millis(3)));
        assert_eq!(p.ahead(240_500_000_000, Duration::from_millis(100)), Some(Pacer::MAX_SLEEP));
        // Behind the wall: no wait.
        assert_eq!(p.ahead(240_510_000_000, Duration::from_millis(550)), None);
        // A snapshot load to an earlier time re-bases too.
        assert_eq!(p.ahead(1_000_000_000, Duration::from_millis(60)), None);
        assert_eq!(p.virtual_ns, 1_000_000_000);
    }

    #[test]
    fn pacing_detects_a_backward_restore_above_the_initial_baseline() {
        let mut p = Pacer::new(0);
        assert_eq!(p.ahead(120_000_000_000, Duration::from_secs(120)), Some(Duration::ZERO));
        assert_eq!(p.ahead(60_000_000_000, Duration::from_secs(120)), None);
        assert_eq!(p.virtual_ns, 60_000_000_000);
        assert_eq!(p.ahead(60_010_000_000, Duration::from_millis(1)), Some(Duration::from_millis(9)));
    }

    #[test]
    fn wifi_env_reads_only_the_wifi_keys() {
        let text = "GEMINI_API_KEY=secret\nWIFI_SSID=\"Home Net\"\nexport WIFI_PASSWORD='hunter2-long'\n# WIFI_SSID=old\n";
        assert_eq!(wifi_spec_from_env(text).unwrap(), "ssid=Home Net,psk=hunter2-long");
        assert_eq!(wifi_spec_from_env("WIFI_SSID=open\nWIFI_PASSWORD=\n").unwrap(), "ssid=open");
        assert!(wifi_spec_from_env("WIFI_PASSWORD=12345678\n").is_err());
        assert!(wifi_spec_from_env("WIFI_SSID=a,b\n").is_err());
        assert!(wifi_spec_from_env("WIFI_SSID=x\nWIFI_PASSWORD=short\n").is_err());
    }

    #[test]
    fn the_saved_mode_applies_per_profile_and_command_line_flags_win() {
        let setup4c = with_avd_network(&parse(&[]), Some("setup"), "note4c").unwrap();
        assert!(setup4c.setup_address && setup4c.nat && setup4c.softap == Some(8080));
        let setup4 = with_avd_network(&parse(&[]), Some("setup"), "note4").unwrap();
        assert!(setup4.setup_address && !setup4.nat && setup4.softap.is_none());
        let user = with_avd_network(&parse(&[]), Some("user"), "note4").unwrap();
        assert!(user.nat && !user.setup_address);
        let off = with_avd_network(&parse(&[]), None, "note4c").unwrap();
        assert!(!off.nat && !off.setup_address && off.softap.is_none());
        let explicit = with_avd_network(&parse(&["--forward", "8081:80"]), Some("setup"), "note4c").unwrap();
        assert!(!explicit.setup_address && !explicit.nat);
        assert!(with_avd_network(&parse(&[]), Some("bridged"), "note4").is_err());
    }
}
