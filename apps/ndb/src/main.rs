//! `ndb` — the NOTE Emulator control tool (proposal §8). G0 provides the
//! offline commands that need no running engine: profiles, ROM import, doctor.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use note_core::{flash, paths, profile, rom, Error};
use note_runtime::Store;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "ndb", version, about = "NOTE Emulator control tool")]
struct Cli {
    /// Machine-readable JSON on stdout; diagnostics stay on stderr.
    #[arg(long, global = true)]
    json: bool,
    /// Running instance or AVD id for instance commands.
    #[arg(short = 's', long, global = true)]
    select: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List hardware profiles.
    Profiles,
    /// Manage the ESP32-S3 mask ROM (never bundled; imported once per user).
    Rom {
        #[command(subcommand)]
        action: RomAction,
    },
    /// Check that everything a device launch needs is present.
    Doctor,
    /// Create and maintain AVDs (offline; a running device keeps the lock).
    Avd {
        #[command(subcommand)]
        action: AvdAction,
    },
    /// List AVDs. Same data as `avd list`.
    Devices,
    /// Export the merged log view from a running instance.
    Logcat {
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        level: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        search: Option<String>,
    },
    /// The running instance's RFC 2217 serial URL (`note-emu --serial-rfc2217 PORT`), for
    /// esptool, idf.py flash/monitor or any serial terminal.
    SerialUrl,
    /// EN-reset the running chip into ROM download mode or back to a normal (SPI flash) boot.
    Boot {
        #[arg(value_parser = ["download", "normal"])]
        mode: String,
    },
    /// Flash a running instance through its serial port with esptool (auto-reset into download
    /// mode, write, verify, hard reset): an ESP-IDF build directory or a merged image at 0x0.
    /// esptool: NOTE_ESPTOOL, or `esptool` / `esptool.py` on PATH.
    Flash { firmware: PathBuf },
    /// Print the GDB command for a running instance's stub (`note-emu --gdb PORT`); `--run`
    /// starts it (NOTE_GDB, or xtensa-esp32s3-elf-gdb / xtensa-esp-elf-gdb on PATH).
    Gdb {
        #[arg(long)]
        elf: Option<PathBuf>,
        #[arg(long)]
        run: bool,
    },
    /// State of a running instance: virtual time, pause, battery, USB, frame sequence.
    Status,
    /// Save the display of a running instance as a PNG (canonical panel pixels).
    Screenshot {
        #[arg(short = 'o', long)]
        out: PathBuf,
    },
    /// Press a button on a running instance (ok, up, down), released after `--hold` ms of
    /// wall time.
    Press {
        button: String,
        #[arg(long, default_value_t = 150)]
        hold: u64,
    },
    /// Set the battery terminal voltage of a running instance (mV).
    Battery { mv: u32 },
    /// Play a PCM WAV (at the firmware's microphone rate) into the microphone from now on.
    Mic { wav: PathBuf },
    /// Plug or unplug the USB cable (supply + charger) and/or the USB host (console).
    Usb {
        #[arg(long, value_parser = ["on", "off"])]
        cable: Option<String>,
        #[arg(long, value_parser = ["on", "off"])]
        host: Option<String>,
    },
    /// Write a firmware into a stopped AVD's flash, region by region like `idf.py flash`
    /// (an ESP-IDF build directory), or from offset 0 for a merged image. Other flash (NVS,
    /// other partitions) is kept; `avd wipe` restores the imported baseline.
    Install { firmware: PathBuf },
    /// Stop a running instance gracefully: it commits flash (and a quick-boot snapshot when
    /// started with `--quick-boot`) and exits.
    Stop,
    /// Full machine snapshots of an AVD (G7). `save`/`load` talk to the running instance;
    /// `list`/`delete` work on the AVD's files and need no instance.
    Snapshot {
        #[command(subcommand)]
        action: SnapshotAction,
    },
    /// Extract the ESP core dump from an AVD's flash (`coredump` partition), for
    /// `esp-coredump info_corefile --core-format raw`. Works on a stopped or running AVD
    /// (the last committed flash).
    Coredump {
        /// Output file (default: <avd>-core.bin).
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum AvdAction {
    /// Import a firmware image into a new AVD.
    Create {
        #[arg(long)]
        profile: String,
        #[arg(long)]
        firmware: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// List AVDs.
    List,
    /// Restore writable flash from the imported baseline.
    Wipe { id: String },
    /// Delete an AVD that is not running.
    Delete { id: String },
    /// Show or set the network mode the AVD starts with: disabled, user, setup, shared.
    Network { id: String, mode: Option<String> },
    /// Show or set whether http://10.0.2.15/ (the station address) opens on this Mac: on, off,
    /// or default (on in setup mode). Needs the network helper.
    StationAddress { id: String, value: Option<String> },
    /// Show or set the dotenv file (mode 0600) whose WIFI_SSID / WIFI_PASSWORD the virtual AP uses.
    Wifi {
        id: String,
        #[arg(long, conflicts_with = "clear")]
        env: Option<PathBuf>,
        #[arg(long)]
        clear: bool,
    },
    /// Map a legacy `.emulator/devices.json`. The file is not modified.
    Import { path: PathBuf, #[arg(long)] profile: String },
}

#[derive(Subcommand)]
enum SnapshotAction {
    /// Save the running machine as NAME (default "default").
    Save { name: Option<String> },
    /// Restore NAME into the running machine.
    Load { name: Option<String> },
    List,
    Delete { name: String },
}

#[derive(Subcommand)]
enum RomAction {
    /// Verify and import esp32s3_rev0_rom.elf (esp-rom-elfs). Without a file: the bundled copy.
    Import { path: Option<PathBuf> },
    /// Show the imported ROM.
    Status,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            if cli.json {
                println!("{}", json!({ "ok": false, "error": { "code": err.code().as_str(), "message": err.to_string() } }));
            } else {
                eprintln!("ndb: {err}");
            }
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> Result<(), Error> {
    match &cli.command {
        Command::Profiles => {
            let profiles = profile::load_all(&profile::profiles_dir())?;
            if cli.json {
                let list: Vec<_> = profiles
                    .iter()
                    .map(|p| json!({ "id": p.id, "name": p.name, "revision": p.revision,
                                     "panel": p.display.panel, "variant": p.display.variant, "format": p.display.format,
                                     "width": p.display.width, "height": p.display.height }))
                    .collect();
                println!("{}", json!({ "ok": true, "profiles": list }));
            } else {
                for p in &profiles {
                    println!("{:<8} {:<8} {:>3}x{:<3} {:?}/{:?}/{:?}  {}", p.id, p.name, p.display.width,
                             p.display.height, p.display.panel, p.display.variant, p.display.format, p.revision);
                }
            }
        }
        Command::Rom { action: RomAction::Import { path } } => {
            let source = match path {
                Some(path) => path.clone(),
                None => rom::bundled().ok_or_else(|| Error::BadRequest("no bundled ROM next to ndb; pass esp32s3_rev0_rom.elf".into()))?,
            };
            let info = rom::import(&source)?;
            report_rom(cli.json, &info, "imported");
        }
        Command::Rom { action: RomAction::Status } => {
            let info = rom::installed()?;
            report_rom(cli.json, &info, "installed");
        }
        Command::Doctor => {
            let profiles = profile::load_all(&profile::profiles_dir());
            let rom = rom::installed();
            let checks = [
                ("profiles", profiles.as_ref().map(|p| format!("{} loaded", p.len())).map_err(|e| e.to_string())),
                ("rom", rom.as_ref().map(|r| format!("{} ({})", r.path.display(), r.release)).map_err(|e| e.to_string())),
            ];
            let ok = checks.iter().all(|(_, r)| r.is_ok());
            if cli.json {
                let items: Vec<_> = checks
                    .iter()
                    .map(|(name, r)| match r {
                        Ok(detail) => json!({ "check": name, "ok": true, "detail": detail }),
                        Err(detail) => json!({ "check": name, "ok": false, "detail": detail }),
                    })
                    .collect();
                println!("{}", json!({ "ok": ok, "checks": items }));
            } else {
                for (name, r) in &checks {
                    match r {
                        Ok(detail) => println!("ok    {name:<9} {detail}"),
                        Err(detail) => println!("FAIL  {name:<9} {detail}"),
                    }
                }
            }
            if !ok {
                // Every failure was already reported in the check list.
                std::process::exit(2);
            }
        }
        Command::Avd { action: AvdAction::Create { profile: profile_id, firmware, name } } => {
            let prof = profile::find(&profile::profiles_dir(), profile_id)?;
            let image = flash::load(firmware, prof.flash_mib as usize * 1024 * 1024)?;
            let store = Store::new(paths::data_home());
            let id = store.create(&prof.id, name.as_deref().unwrap_or(&prof.name), &image.bytes, firmware)?;
            finish(cli.json, json!({ "ok": true, "id": id, "profile": prof.id }));
        }
        Command::Avd { action: AvdAction::List } | Command::Devices => {
            let listed = Store::new(paths::data_home()).list()?;
            if cli.json {
                let avds: Vec<_> = listed.iter().map(|a| json!({
                    "id": a.config.id, "name": a.config.name, "profile": a.config.profile,
                    "sha256": a.config.sha256, "network": a.config.network.as_deref().unwrap_or("disabled")
                })).collect();
                finish(cli.json, json!({ "ok": true, "avds": avds }));
            } else {
                for a in &listed {
                    println!("{:<38} {:<16} {}", a.config.id, a.config.profile, a.config.name);
                }
            }
        }
        Command::Avd { action: AvdAction::Wipe { id } } => {
            Store::new(paths::data_home()).wipe(id)?;
            finish(cli.json, json!({ "ok": true, "wiped": id }));
        }
        Command::Avd { action: AvdAction::Network { id, mode } } => {
            let store = Store::new(paths::data_home());
            let config = match mode {
                Some(mode) => store.set_network(id, mode)?,
                None => store.config(id)?,
            };
            let mode = config.network.as_deref().unwrap_or("disabled");
            if cli.json {
                finish(true, json!({ "ok": true, "id": id, "network": mode }));
            } else {
                println!("{mode}");
            }
        }
        Command::Avd { action: AvdAction::StationAddress { id, value } } => {
            let store = Store::new(paths::data_home());
            let config = match value.as_deref() {
                None => store.config(id)?,
                Some("on") => store.set_station_address(id, Some(true))?,
                Some("off") => store.set_station_address(id, Some(false))?,
                Some("default") => store.set_station_address(id, None)?,
                Some(other) => return Err(Error::BadRequest(format!("station address must be on, off or default, not {other:?}"))),
            };
            let on = config.wants_station_address();
            if cli.json {
                finish(true, json!({ "ok": true, "id": id, "station_address": on, "pinned": config.station_address }));
            } else {
                println!("{}", if on { "on" } else { "off" });
            }
        }
        Command::Avd { action: AvdAction::Wifi { id, env, clear } } => {
            let store = Store::new(paths::data_home());
            let mut config = store.config(id)?;
            if *clear {
                config.wifi_env = None;
                store.set_config(&config)?;
            } else if let Some(env) = env {
                let path = std::fs::canonicalize(env).map_err(|e| Error::io("wifi env", e))?;
                let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&path).map_err(|e| Error::io("wifi env", e))?.permissions());
                if mode & 0o077 != 0 {
                    return Err(Error::BadRequest(format!("{} must be mode 0600", path.display())));
                }
                config.wifi_env = Some(path.display().to_string());
                store.set_config(&config)?;
            }
            let shown = config.wifi_env.clone().unwrap_or_default();
            if cli.json { finish(true, json!({ "ok": true, "id": id, "wifi_env": config.wifi_env })); } else { println!("{shown}"); }
        }
        Command::Avd { action: AvdAction::Delete { id } } => {
            Store::new(paths::data_home()).delete(id)?;
            finish(cli.json, json!({ "ok": true, "deleted": id }));
        }
        Command::Avd { action: AvdAction::Import { path, profile: profile_id } } => {
            let prof = profile::find(&profile::profiles_dir(), profile_id)?;
            let text = std::fs::read_to_string(path).map_err(|e| note_core::Error::io(format!("read {}", path.display()), e))?;
            let imported = note_runtime::import(&Store::new(paths::data_home()), &text, &prof.id, prof.flash_mib as usize * 1024 * 1024)?;
            finish(cli.json, json!({
                "ok": true,
                "created": imported.accept.iter().map(|a| &a.name).collect::<Vec<_>>(),
                "review": imported.review.iter().map(|r| json!({ "name": r.name, "reason": r.reason })).collect::<Vec<_>>()
            }));
        }
        Command::SerialUrl => {
            let url = serial_url(cli)?;
            if cli.json { println!("{}", json!({ "ok": true, "url": url })); } else { println!("{url}"); }
        }
        Command::Boot { mode } => {
            let value = call(select(cli, "boot")?, json!({ "method": "boot.reset", "mode": mode }))?.0;
            finish(cli.json, json!({ "ok": true, "mode": value["mode"] }));
            if !cli.json { println!("reset into {mode} boot"); }
        }
        Command::Flash { firmware } => {
            let url = serial_url(cli)?;
            let esptool = tool_on_path("NOTE_ESPTOOL", &["esptool", "esptool.py"])
                .ok_or_else(|| Error::BadRequest("esptool not found: set NOTE_ESPTOOL or put esptool on PATH".into()))?;
            let mut args: Vec<String> = vec!["--chip".into(), "esp32s3".into(), "--port".into(), url, "write-flash".into()];
            if firmware.is_dir() {
                let text = std::fs::read_to_string(firmware.join("flasher_args.json")).map_err(|e| Error::io("read flasher_args.json", e))?;
                let v: Value = serde_json::from_str(&text).map_err(|e| Error::BadRequest(e.to_string()))?;
                for (offset, file) in v["flash_files"].as_object().ok_or_else(|| Error::BadRequest("flasher_args.json has no flash_files".into()))? {
                    args.push(offset.clone());
                    args.push(firmware.join(file.as_str().unwrap_or("")).display().to_string());
                }
            } else {
                args.push("0x0".into());
                args.push(firmware.display().to_string());
            }
            let status = std::process::Command::new(&esptool).args(&args).status().map_err(|e| Error::io(format!("run {}", esptool.display()), e))?;
            if !status.success() {
                return Err(Error::BadRequest(format!("esptool failed ({status})")));
            }
        }
        Command::Gdb { elf, run } => {
            let value = call(select(cli, "gdb")?, json!({ "method": "status" }))?.0;
            let target = value["gdb"].as_str().ok_or_else(|| Error::BadRequest("this instance has no GDB stub: start it with note-emu --gdb PORT".into()))?.to_string();
            let gdb = tool_on_path("NOTE_GDB", &["xtensa-esp32s3-elf-gdb", "xtensa-esp-elf-gdb", "xtensa-esp-elf-gdb-no-python"]);
            let name = gdb.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "xtensa-esp-elf-gdb".into());
            let mut args = vec!["-ex".to_string(), format!("target remote {target}")];
            if let Some(elf) = elf { args.push(elf.display().to_string()); }
            if *run {
                let gdb = gdb.ok_or_else(|| Error::BadRequest("GDB not found: set NOTE_GDB or put xtensa-esp-elf-gdb on PATH".into()))?;
                let status = std::process::Command::new(gdb).args(&args).status().map_err(|e| Error::io("run gdb", e))?;
                if !status.success() { return Err(Error::BadRequest(format!("gdb exited ({status})"))); }
            } else if cli.json {
                let command: Vec<String> = std::iter::once(name).chain(args).collect();
                println!("{}", json!({ "ok": true, "target": target, "command": command }));
            } else {
                println!("{name} {}", args.iter().map(|a| if a.contains(' ') { format!("'{a}'") } else { a.clone() }).collect::<Vec<_>>().join(" "));
            }
        }
        Command::Status => {
            let value = call(select(cli, "status")?, json!({ "method": "status" }))?.0;
            if cli.json {
                println!("{value}");
            } else {
                println!("profile {}  t={:.3}s  paused={}  battery={} mV  usb cable={} host={}  frame {}",
                         value["profile"].as_str().unwrap_or("?"), value["virtual_ns"].as_u64().unwrap_or(0) as f64 / 1e9,
                         value["paused"], value["battery_mv"], value["usb_cable"], value["usb_host"], value["frame_seq"]);
            }
        }
        Command::Screenshot { out } => {
            let (value, messages) = call(select(cli, "screenshot")?, json!({ "method": "display.get" }))?;
            let frame = messages.iter().find(|m| m.kind == note_protocol::Kind::Frame)
                .ok_or_else(|| Error::BadRequest("no frame in the reply".into()))?;
            let (_, pixels) = note_protocol::frame::decode(&frame.payload).map_err(|e| Error::BadRequest(e.to_string()))?;
            let status = call(select(cli, "screenshot")?, json!({ "method": "status" }))?.0;
            let prof = profile::find(&profile::profiles_dir(), status["profile"].as_str().unwrap_or(""))?;
            let mut capture = note_runtime::FrameCapture::from_display(&prof.display);
            capture.commit(&pixels, 0);
            let png = capture.png(0).map_err(|e| Error::io("encode png", e))?;
            std::fs::write(out, png).map_err(|e| Error::io(format!("write {}", out.display()), e))?;
            finish(cli.json, json!({ "ok": true, "path": out, "seq": value["seq"] }));
            if !cli.json { println!("{}", out.display()); }
        }
        Command::Press { button, hold } => {
            let sock = select(cli, "press")?;
            call(sock.clone(), json!({ "method": "button.down", "button": button }))?;
            std::thread::sleep(std::time::Duration::from_millis(*hold));
            call(sock, json!({ "method": "button.up", "button": button }))?;
            finish(cli.json, json!({ "ok": true, "button": button, "hold_ms": hold }));
        }
        Command::Battery { mv } => {
            let value = call(select(cli, "battery")?, json!({ "method": "battery.set", "mv": mv }))?.0;
            finish(cli.json, json!({ "ok": true, "mv": value["mv"] }));
        }
        Command::Mic { wav } => {
            let path = std::fs::canonicalize(wav).map_err(|e| Error::io("read wav", e))?;
            let value = call(select(cli, "mic")?, json!({ "method": "audio.mic", "path": path }))?.0;
            finish(cli.json, json!({ "ok": true, "samples": value["samples"] }));
        }
        Command::Usb { cable, host } => {
            let on = |v: &Option<String>| v.as_deref().map(|v| v == "on");
            let value = call(select(cli, "usb")?, json!({ "method": "power.usb", "cable": on(cable), "host": on(host) }))?.0;
            if cli.json { println!("{value}"); } else { println!("usb cable={} host={}", value["cable"], value["host"]); }
        }
        Command::Install { firmware } => {
            let id = cli.select.as_deref().ok_or_else(|| Error::BadRequest("install needs -s <avd>".into()))?;
            let store = Store::new(paths::data_home());
            let config = store.config(id)?;
            let prof = profile::find(&profile::profiles_dir(), &config.profile)?;
            let size = prof.flash_mib as usize * 1024 * 1024;
            let _lock = store.lock(id)?; // refuses while the AVD runs
            let image = flash::load(firmware, size)?;
            let mut current = store.flash(id)?;
            let regions: Vec<(usize, usize)> = if image.segments.is_empty() {
                let len = std::fs::metadata(firmware).map(|m| m.len() as usize).unwrap_or(size).min(size);
                vec![(0, len)]
            } else {
                image.segments.iter().map(|(off, _, len)| (*off, *len)).collect()
            };
            for &(off, len) in &regions {
                current[off..off + len].copy_from_slice(&image.bytes[off..off + len]);
            }
            store.commit(id, &current)?;
            let written: usize = regions.iter().map(|r| r.1).sum();
            finish(cli.json, json!({ "ok": true, "avd": id, "regions": regions.len(), "bytes": written }));
            if !cli.json { println!("installed {} region(s), {written} bytes, into {id}", regions.len()); }
        }
        Command::Stop => {
            let id = cli.select.as_deref().ok_or_else(|| Error::BadRequest("stop needs -s <avd or instance>".into()))?;
            let sock = find_socket(id)?;
            let payload = json!({ "method": "stop" });
            note_runtime::transact(&sock, 1, &serde_json::to_vec(&payload).unwrap()).map_err(|e| Error::io("stop", e))?;
            // Wait for the instance to unpublish itself (flash committed).
            for _ in 0..600 {
                if find_socket(id).is_err() {
                    finish(cli.json, json!({ "ok": true, "stopped": id }));
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            return Err(Error::BadRequest(format!("{id} did not stop within 60 s")));
        }
        Command::Snapshot { action } => {
            let id = cli.select.as_deref().ok_or_else(|| Error::BadRequest("snapshot needs -s <avd>".into()))?;
            match action {
                SnapshotAction::List | SnapshotAction::Delete { .. } => {
                    let dir = Store::new(paths::data_home()).snapshot_dir(id)?;
                    if let SnapshotAction::Delete { name } = action {
                        if !note_runtime::session::valid_snapshot_name(name) {
                            return Err(Error::BadRequest(format!("bad snapshot name {name:?}")));
                        }
                        std::fs::remove_file(dir.join(format!("{name}.snap"))).map_err(|e| Error::io(format!("delete {name}"), e))?;
                        finish(cli.json, json!({ "ok": true, "deleted": name }));
                    } else {
                        let list = note_runtime::session::list_snapshots(&dir);
                        if cli.json {
                            println!("{}", json!({ "ok": true, "snapshots": list }));
                        } else {
                            for s in &list {
                                println!("{:<24} {:>6.1} MB", s["name"].as_str().unwrap_or(""), s["bytes"].as_f64().unwrap_or(0.0) / 1e6);
                            }
                        }
                    }
                }
                SnapshotAction::Save { name } | SnapshotAction::Load { name } => {
                    let method = if matches!(action, SnapshotAction::Save { .. }) { "snapshot.save" } else { "snapshot.load" };
                    let sock = find_socket(id)?;
                    let payload = json!({ "method": method, "name": name.as_deref().unwrap_or("default") });
                    let msgs = note_runtime::transact(&sock, 1, &serde_json::to_vec(&payload).unwrap()).map_err(|e| Error::io("snapshot", e))?;
                    let body = msgs.into_iter().find(|m| m.kind == note_protocol::Kind::Response).ok_or_else(|| Error::BadRequest("no response".into()))?;
                    let value: Value = serde_json::from_slice(&body.payload).map_err(|e| Error::BadRequest(e.to_string()))?;
                    if value.get("ok") != Some(&json!(true)) {
                        let message = value.pointer("/error/message").and_then(|m| m.as_str()).map(str::to_string);
                        return Err(Error::BadRequest(message.unwrap_or_else(|| value.to_string())));
                    }
                    if cli.json { println!("{value}"); } else { println!("{method} {}: ok", name.as_deref().unwrap_or("default")); }
                }
            }
        }
        Command::Coredump { out } => {
            let id = cli.select.as_deref().ok_or_else(|| Error::BadRequest("coredump needs -s <avd>".into()))?;
            let image = Store::new(paths::data_home()).flash(id)?;
            let core = flash::coredump(&image).ok_or_else(|| {
                Error::BadRequest(format!("{id}: no core dump (no coredump partition, or it is erased)"))
            })?;
            // The partition is padded with erased flash; the ELF core's own length is not needed
            // by esp-coredump, which reads the header.
            let out = out.clone().unwrap_or_else(|| PathBuf::from(format!("{id}-core.bin")));
            std::fs::write(&out, core).map_err(|e| Error::io(format!("write {}", out.display()), e))?;
            if cli.json {
                println!("{}", json!({ "ok": true, "avd": id, "path": out, "bytes": core.len() }));
            } else {
                println!("{}", out.display());
                eprintln!("decode: esp-coredump --chip esp32s3 info_corefile --core {} --core-format raw <app.elf>", out.display());
            }
        }
        Command::Logcat { source, level, tag, search } => {
            let select = cli.select.as_deref().ok_or_else(|| Error::BadRequest("logcat needs -s <instance>".into()))?;
            let sock = find_socket(select)?;
            let payload = json!({ "method": "logs.export", "source": source, "level": level, "tag": tag, "search": search, "view": true });
            let msgs = note_runtime::transact(&sock, 1, &serde_json::to_vec(&payload).unwrap()).map_err(|e| Error::io("logcat", e))?;
            let body = msgs.into_iter().find(|m| m.kind == note_protocol::Kind::Response).ok_or_else(|| Error::BadRequest("no response".into()))?;
            let value: Value = serde_json::from_slice(&body.payload).map_err(|e| Error::BadRequest(e.to_string()))?;
            if cli.json {
                println!("{value}");
            } else if value.get("ok") == Some(&json!(true)) {
                for line in value.get("lines").and_then(|v| v.as_array()).into_iter().flatten() {
                    println!("{}", line.get("text").and_then(|t| t.as_str()).unwrap_or(""));
                }
            } else {
                return Err(Error::BadRequest(value.to_string()));
            }
        }
    }
    Ok(())
}

fn serial_url(cli: &Cli) -> std::result::Result<String, Error> {
    let value = call(select(cli, "serial-url")?, json!({ "method": "status" }))?.0;
    value["serial_url"].as_str().map(str::to_string)
        .ok_or_else(|| Error::BadRequest("this instance has no serial port: start it with note-emu --serial-rfc2217 PORT".into()))
}

/// `env` if set, else the first of `names` found on PATH.
fn tool_on_path(env: &str, names: &[&str]) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(env) {
        return Some(PathBuf::from(p));
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The running instance's control socket for `-s`.
fn select(cli: &Cli, what: &str) -> std::result::Result<PathBuf, Error> {
    let id = cli.select.as_deref().ok_or_else(|| Error::BadRequest(format!("{what} needs -s <avd or instance>")))?;
    find_socket(id)
}

/// One request; the JSON response (an error reply becomes `Err` with its message) and every
/// message that came with it (frames, audio).
fn call(sock: PathBuf, payload: Value) -> std::result::Result<(Value, Vec<note_protocol::Message>), Error> {
    let msgs = note_runtime::transact(&sock, 1, &serde_json::to_vec(&payload).unwrap()).map_err(|e| Error::io("control socket", e))?;
    let body = msgs.iter().find(|m| m.kind == note_protocol::Kind::Response).ok_or_else(|| Error::BadRequest("no response".into()))?;
    let value: Value = serde_json::from_slice(&body.payload).map_err(|e| Error::BadRequest(e.to_string()))?;
    if value.get("ok") == Some(&json!(false)) {
        let message = value.pointer("/error/message").and_then(|m| m.as_str()).map(str::to_string);
        return Err(Error::BadRequest(message.unwrap_or_else(|| value.to_string())));
    }
    Ok((value, msgs))
}

fn finish(json_out: bool, value: Value) {
    if json_out {
        println!("{value}");
    } else if let Some(id) = value.get("id").and_then(|v| v.as_str()) {
        println!("{id}");
    }
}

fn find_socket(select: &str) -> std::result::Result<PathBuf, Error> {
    let run = paths::data_home().join("run");
    if !run.exists() {
        return Err(Error::BadRequest(format!("no running instance {select}")));
    }
    for entry in std::fs::read_dir(&run).map_err(|e| Error::io("read run dir", e))? {
        let entry = entry.map_err(|e| Error::io("read run entry", e))?;
        let text = std::fs::read_to_string(entry.path().join("instance.json")).unwrap_or_default();
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let id = value.get("instance").and_then(|v| v.as_str()).unwrap_or("");
        let avd = value.get("avd").and_then(|v| v.as_str()).unwrap_or("");
        if id == select || avd == select {
            let sock = value.get("sock").and_then(|v| v.as_str()).unwrap_or("");
            if sock.is_empty() {
                break;
            }
            return Ok(PathBuf::from(sock));
        }
    }
    Err(Error::BadRequest(format!("no running instance {select}")))
}

fn report_rom(json_out: bool, info: &rom::RomInfo, verb: &str) {
    if json_out {
        println!("{}", json!({ "ok": true, "rom": { "path": info.path, "sha256": info.sha256, "release": info.release } }));
    } else {
        println!("ROM {verb}: {} ({}, sha256 {})", info.path.display(), info.release, info.sha256);
    }
}
