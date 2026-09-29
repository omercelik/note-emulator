//! Unmodified-firmware regression tests (Spec §17, layer 3). They need the imported ROM
//! (imported from `third_party/` automatically) and firmware images, some private (`.tools/private`), so they are `#[ignore]`d:
//! `cargo test --release -p note-machine --test firmware -- --ignored`.

use std::path::{Path, PathBuf};

use note_core::{flash, profile, rom};
use note_machine::{NoteMachine, RadioConfig, SliceEnd, CPU_HZ};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Private firmware (not redistributable): `NOTE_PRIVATE_DIR`, default `.tools/private` in the
/// repo (gitignored). It keeps the layout of the original QEMU project: `.device-backup/` holds
/// flash dumps, `.device-tools/` firmware builds and releases.
fn private_dir() -> PathBuf {
    std::env::var_os("NOTE_PRIVATE_DIR").map(PathBuf::from).unwrap_or_else(|| repo().join(".tools/private"))
}

fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c9dc5u32, |h, &b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

fn boot(profile_id: &str, firmware: &Path) -> NoteMachine {
    assert!(firmware.exists(), "fixture missing: {}", firmware.display());
    let p = profile::find(&profile::profiles_dir(), profile_id).unwrap();
    let rom = std::fs::read(rom::installed().expect("run `ndb rom import` first").path).unwrap();
    let image = flash::load(firmware, (p.flash_mib as usize) << 20).unwrap();
    NoteMachine::new(&p, &rom, &image, [0x02, 0x4e, 0x4f, 0x54, 0x45, 0x04], 1_790_546_398, &RadioConfig::default()).unwrap()
}

fn run_until(nm: &mut NoteMachine, seconds: f64) {
    assert_eq!(nm.run_until((seconds * CPU_HZ as f64) as u64), SliceEnd::Reached);
}

fn note4_demo() -> NoteMachine {
    boot("note4", &repo().join("third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin"))
}

#[test]
#[ignore]
fn note4_demo_boots_to_its_menu_and_navigates_with_partial_refresh() {
    let mut nm = note4_demo();
    run_until(&mut nm, 3.0);
    let menu = {
        let st = nm.board.lock();
        assert_eq!(st.panel.refreshes.len(), 2, "splash + menu");
        assert!(st.panel.readbacks >= 2, "temperature readback over 3-wire MISO");
        fnv1a(st.panel.visible())
    };
    assert_eq!(menu, MENU_HASH, "menu frame pixels changed");
    nm.set_button("down", true).unwrap();
    run_until(&mut nm, 3.1);
    nm.set_button("down", false).unwrap();
    run_until(&mut nm, 4.0);
    let st = nm.board.lock();
    let last = st.panel.refreshes.last().unwrap();
    assert_eq!(format!("{:?}", last.kind), "Partial");
    assert_eq!(last.rect, (0, 36, 400, 234));
    assert_ne!(fnv1a(st.panel.visible()), menu);
    assert!(st.panel.diagnostics.is_empty(), "{:?}", st.panel.diagnostics);
}

/// Pixel hash of the NOTE4 demo main menu (v1.0.0), captured 2026-09-27.
const MENU_HASH: u32 = 3_467_383_573;

fn console_text(nm: &mut NoteMachine) -> String {
    let c = nm.take_console();
    let mut text = String::from_utf8_lossy(&c.uart0).into_owned();
    text.push_str(&String::from_utf8_lossy(&c.usb));
    text
}

/// Lines that show a join or an address, with the example's password line removed.
fn link_lines(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            !lower.contains("password")
                && (lower.contains("connected with")
                    || lower.contains("got ip:")
                    || lower.contains("sta ip:")
                    || lower.contains("retry to connect")
                    || lower.contains("wifi:state"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Unmodified ESP-IDF 6.0 `wifi/getting_started/station` (open SSID `esp32sim`, the virtual AP),
/// built by `scripts/build-fixtures.sh wifi-station`; `WIFI_STATION_BUILD` overrides the path.
#[test]
#[ignore = "needs the ROM and .tools/fixtures/wifi-station (scripts/build-fixtures.sh wifi-station)"]
fn station_joins_then_rejoins_after_deauth() {
    let dir = std::env::var_os("WIFI_STATION_BUILD")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join(".tools/fixtures/wifi-station"));
    assert!(dir.join("flasher_args.json").is_file(),
            "no station build at {} — run scripts/build-fixtures.sh wifi-station", dir.display());
    let mut nm = boot("note4", &dir);
    nm.set_jit(true);
    let mut log = String::new();
    let mut dropped = false;
    let mut at_drop = (0u64, 0u64);
    for step in 1..=90 {
        run_until(&mut nm, step as f64 * 0.5);
        log.push_str(&console_text(&mut nm));
        let (associations, acks) = nm.station_net();
        let got = log.matches("got ip:10.0.2.15").count();
        let sta = log.matches("sta ip: 10.0.2.15").count();
        if !dropped && associations >= 1 && acks >= 1 && (got >= 1 || sta >= 1) {
            at_drop = (associations, acks);
            assert!(nm.disconnect_station(), "the station had authenticated");
            dropped = true;
            continue;
        }
        if dropped {
            let (associations, acks) = nm.station_net();
            let got = log.matches("got ip:10.0.2.15").count();
            let sta = log.matches("sta ip: 10.0.2.15").count();
            let connected = log.matches("connected with").count();
            if associations > at_drop.0 && acks > at_drop.1 && connected >= 2 && (got >= 2 || sta >= 2) {
                let probes = nm.m.bus.periph.wifi.ap.as_ref().map(|ap| ap.stats.1).unwrap_or(0);
                assert!(probes >= 1, "scan sent no probe; the driver never asked the virtual AP");
                eprintln!(
                    "rejoins at {:.3}s virtual, associations {} -> {}, dhcp acks {} -> {}, probes {probes}\n{}",
                    nm.seconds(),
                    at_drop.0,
                    associations,
                    at_drop.1,
                    acks,
                    link_lines(&log)
                );
                return;
            }
        }
    }
    let (associations, acks) = nm.station_net();
    panic!(
        "station did not join twice (dropped={dropped}, at_drop={at_drop:?}, now=({associations}, {acks}))\n{}",
        link_lines(&log)
    );
}

fn press(nm: &mut NoteMachine, button: &str, at: f64) {
    run_until(nm, at);
    nm.set_button(button, true).unwrap();
    run_until(nm, at + 0.1);
    nm.set_button(button, false).unwrap();
}

fn gray_levels(visible: &[u8]) -> usize {
    let mut seen = [false; 16];
    for b in visible {
        seen[(b >> 4) as usize] = true;
        seen[(b & 15) as usize] = true;
    }
    seen.iter().filter(|s| **s).count()
}

/// DISP-02 on unmodified firmware: Display Gallery → 16-GRAY / 4BPP runs five external-waveform
/// passes; each must be a recognised pass and the result must use all 16 levels.
#[test]
#[ignore]
fn note4_demo_draws_the_16_gray_gallery_scene() {
    let mut nm = note4_demo();
    press(&mut nm, "down", 4.0); // DISPLAY GALLERY
    press(&mut nm, "ok", 5.5);
    press(&mut nm, "down", 7.5); // PARTIAL
    press(&mut nm, "down", 8.5); // 16-GRAY / 4BPP
    press(&mut nm, "ok", 10.0);
    run_until(&mut nm, 13.0);
    let st = nm.board.lock();
    let grays = st.panel.refreshes.iter().filter(|r| format!("{:?}", r.kind) == "Gray").count();
    assert_eq!(grays, 5, "five gray passes");
    assert!(st.panel.diagnostics.is_empty(), "{:?}", st.panel.diagnostics);
    assert_eq!(gray_levels(st.panel.visible()), 16);
    assert_eq!(fnv1a(st.panel.visible()), GRAY_SCENE_HASH, "gray scene pixels changed");
}

/// Pixel hash of the NOTE4 demo 16-gray gallery scene (v1.0.0) after its fifth pass.
const GRAY_SCENE_HASH: u32 = 3_627_567_340;

/// POWER-01 on unmodified firmware: holding DOWN clears the panel and opens the battery latch;
/// without external power the board goes dark (not a reset) and the e-paper keeps its image.
/// Waking is a cold power-on: the ROM reports POWERON and the app boots to its splash again.
#[test]
#[ignore]
fn note4_demo_long_hold_down_powers_off_and_wakes_cold() {
    let mut nm = note4_demo();
    run_until(&mut nm, 4.0);
    nm.set_external_power(false);
    nm.set_button("down", true).unwrap();
    let end = nm.run_until(12 * CPU_HZ);
    assert_eq!(end, SliceEnd::PoweredOff, "the firmware opened the latch");
    let off_at = nm.seconds();
    assert!(off_at < 9.0, "powered off at {off_at:.2}s");
    let log = console_text(&mut nm);
    assert!(log.contains("cutting battery latch"), "{log}");
    let (refreshes, image) = {
        let st = nm.board.lock();
        (st.panel.refreshes.len(), st.panel.visible().to_vec())
    };
    nm.set_button("down", false).unwrap();
    assert_eq!(nm.run_until(nm.cycles() + CPU_HZ), SliceEnd::PoweredOff, "stays off without a wake");
    assert_eq!(nm.board.lock().panel.visible(), &image[..], "e-paper retains the last image");

    nm.wake();
    let t = nm.seconds();
    run_until(&mut nm, t + 3.0);
    let log = console_text(&mut nm);
    assert!(log.contains("rst:0x1 (POWERON)"), "cold power-on, not a chip reset:\n{log}");
    assert_eq!(nm.power_cycles, 1);
    let st = nm.board.lock();
    assert!(st.panel.refreshes.len() >= refreshes + 2, "splash and menu drawn again");
}

/// FW-02 on the private factory NOTE4C dump (private): the
/// factory flow passes RF (virtual AP), audio (speaker→mic loopback) and RTC, passes charging
/// once USB is plugged, fails NFC (not modelled), and on the result screen a long press of OK
/// makes the firmware open the battery latch.
#[test]
#[ignore = "needs the private factory dump in $NOTE_PRIVATE_DIR/.device-backup/factory-2026-09-23.bin"]
fn note4c_factory_flow_and_long_press_ok_powers_off() {
    let private = private_dir();
    let mut nm = boot("note4c", &private.join(".device-backup/factory-2026-09-23.bin"));
    run_until(&mut nm, 12.0);
    nm.set_external_power(true);
    nm.set_charger(true, None);
    run_until(&mut nm, 14.5);
    nm.set_external_power(false);
    nm.set_charger(false, None);
    run_until(&mut nm, 15.0);
    let log = console_text(&mut nm);
    for step in ["type=audio result=PASS", "step=RTC 测试 state=2", "step=充电测试 state=2", "step=NFC 测试 result=FAIL"] {
        assert!(log.contains(step), "factory log is missing {step:?}");
    }
    // The audio test's tone reaches the host speaker stream through the enabled amplifier.
    let (rate, _, samples) = note_machine::replay::Guest::speaker(&nm, Some(0), usize::MAX);
    let loud = samples.iter().filter(|s| s.unsigned_abs() > 1000).count();
    assert!(rate > 0 && loud > 100, "rate {rate}, {loud} audible of {} samples", samples.len());
    assert_eq!(nm.board.lock().panel.refreshes.len(), 2, "test screen, then the result screen");
    nm.set_button("ok", true).unwrap();
    assert_eq!(nm.run_until(20 * CPU_HZ), SliceEnd::PoweredOff, "long-press OK powers the board off");
    assert!(nm.seconds() < 17.5, "powered off at {:.2}s", nm.seconds());
}

/// NET-01 on unmodified firmware: ESP-IDF 6.0 `protocols/http_server/simple`
/// (`scripts/build-fixtures.sh http-server`) joins the virtual AP; a loopback forward carries
/// a host HTTP client to its port 80 with outbound NAT off.
#[test]
#[ignore = "needs the ROM and .tools/fixtures/http-server (scripts/build-fixtures.sh http-server)"]
fn station_http_server_answers_through_a_loopback_forward() {
    use std::io::Write;
    let dir = repo().join(".tools/fixtures/http-server");
    assert!(dir.join("flasher_args.json").is_file(), "run scripts/build-fixtures.sh http-server");
    let mut nm = boot("note4", &dir);
    let addr = nm.listen_forward("127.0.0.1:0".parse().unwrap(), 80).unwrap();
    let mut log = String::new();
    let mut t = 0.0;
    while !log.contains("Registering URI handlers") {
        t += 0.25;
        assert!(t < 20.0, "server did not start:\n{log}");
        run_until(&mut nm, t);
        log.push_str(&console_text(&mut nm));
    }
    let client = std::thread::spawn(move || {
        let mut out = Vec::new();
        for (req, _) in [("GET /hello HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n", ()),
                         ("POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: 20\r\nConnection: close\r\n\r\nnote-emulator NET-01", ())] {
            let mut s = std::net::TcpStream::connect(addr).unwrap();
            s.set_read_timeout(Some(std::time::Duration::from_secs(20))).unwrap();
            s.write_all(req.as_bytes()).unwrap();
            out.push(read_http_response(&mut s));
        }
        out
    });
    // Real-time pacing, as `note-emu --realtime` does for network sessions: the host client
    // and the guest's TCP timers must see comparable time.
    let started = std::time::Instant::now();
    let t0 = t;
    while !client.is_finished() {
        t += 0.05;
        assert!(t < t0 + 30.0, "HTTP exchange did not finish");
        run_until(&mut nm, t);
        if let Some(ahead) = std::time::Duration::from_secs_f64(t - t0).checked_sub(started.elapsed()) {
            std::thread::sleep(ahead);
        }
    }
    let replies = client.join().unwrap();
    assert!(replies[0].starts_with("HTTP/1.1 200 OK"), "{}", replies[0]);
    assert!(replies[0].ends_with("Hello World!"), "{}", replies[0]);
    assert!(replies[1].starts_with("HTTP/1.1 200 OK"), "{}", replies[1]);
    assert!(replies[1].contains("\r\n14\r\nnote-emulator NET-01\r\n"), "echoed body: {}", replies[1]);
}

/// One HTTP/1.1 response, framed by Content-Length or chunked encoding (ESP-IDF httpd keeps
/// the connection open).
fn read_http_response(s: &mut std::net::TcpStream) -> String {
    use std::io::Read;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let text = String::from_utf8_lossy(&buf).into_owned();
        if let Some(end) = text.find("\r\n\r\n") {
            if text[..end].to_ascii_lowercase().contains("transfer-encoding: chunked") {
                if text[end..].contains("\r\n0\r\n\r\n") {
                    return text;
                }
            } else {
                let len = text[..end]
                .lines()
                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                .unwrap_or(0);
                if buf.len() >= end + 4 + len {
                    return text;
                }
            }
        }
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return String::from_utf8_lossy(&buf).into_owned(),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// Crypto accelerator diagnostic (`fixtures/diag-fw/crypto`, `scripts/build-fixtures.sh
/// diag-fw/crypto`): SHA-2 and AES-GCM through PSA and the S3 hardware drivers, compared with
/// RustCrypto. Every mismatch is listed.
#[test]
#[ignore = "needs the ROM and .tools/fixtures/diag-crypto (scripts/build-fixtures.sh diag-fw/crypto)"]
fn crypto_accelerators_match_reference_implementations() {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use sha2::Digest;
    let dir = repo().join(".tools/fixtures/diag-crypto");
    assert!(dir.join("flasher_args.json").is_file(), "run scripts/build-fixtures.sh diag-fw/crypto");
    let mut nm = boot("note4c", &dir);
    let mut log = String::new();
    let mut t = 0.0;
    while !log.contains("DIAG done") {
        t += 0.5;
        assert!(t < 60.0, "diagnostic did not finish:\n{log}");
        run_until(&mut nm, t);
        log.push_str(&console_text(&mut nm));
    }
    let input: Vec<u8> = (0..8192u32).map(|i| (i.wrapping_mul(7).wrapping_add(3)) as u8).collect();
    let key = [0xfe, 0xff, 0xe9, 0x92, 0x86, 0x65, 0x73, 0x1c, 0x6d, 0x6a, 0x8f, 0x94, 0x67, 0x30, 0x83, 0x08,
               0xfe, 0xff, 0xe9, 0x92, 0x86, 0x65, 0x73, 0x1c, 0x6d, 0x6a, 0x8f, 0x94, 0x67, 0x30, 0x83, 0x08];
    let nonce = [0xca, 0xfe, 0xba, 0xbe, 0xfa, 0xce, 0xdb, 0xad, 0xde, 0xca, 0xf8, 0x88];
    let aad = [0xfe, 0xed, 0xfa, 0xce, 0xde, 0xad, 0xbe, 0xef, 0xfe, 0xed, 0xfa, 0xce, 0xde, 0xad, 0xbe, 0xef, 0xab, 0xad, 0xda, 0xd2];
    let hexs = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let mut checked = 0;
    let mut bad = Vec::new();
    // DIAG lines can be twinned on UART0 and USB; check each distinct line once.
    let mut seen = std::collections::BTreeSet::new();
    for line in log.lines().filter_map(|l| l.find("DIAG ").map(|i| &l[i..])) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() != 4 || !seen.insert(line.to_string()) {
            continue;
        }
        let (what, len, got) = (f[1], f[2].parse::<usize>().unwrap(), f[3]);
        if what.ends_with("-rt") || what.ends_with("-forged") {
            checked += 1;
            if got != "ok" {
                bad.push(format!("{what} len {len}: {got}"));
            }
            continue;
        }
        let data = &input[..len];
        let base = what.trim_end_matches("-inc2").trim_end_matches("-inc").trim_end_matches("-clone");
        let hmac_of = |data: &[u8]| -> String {
            use hmac::Mac;
            let key = [0x0bu8; 20];
            match base {
                "hmac256" => hexs(&<hmac::Hmac<sha2::Sha256> as Mac>::new_from_slice(&key).unwrap().chain_update(data).finalize().into_bytes()),
                "hmac384" => hexs(&<hmac::Hmac<sha2::Sha384> as Mac>::new_from_slice(&key).unwrap().chain_update(data).finalize().into_bytes()),
                _ => hexs(&<hmac::Hmac<sha2::Sha512> as Mac>::new_from_slice(&key).unwrap().chain_update(data).finalize().into_bytes()),
            }
        };
        let want = match base {
            "sha256" => hexs(&sha2::Sha256::digest(data)),
            "sha384" => hexs(&sha2::Sha384::digest(data)),
            "sha512" => hexs(&sha2::Sha512::digest(data)),
            b if b.starts_with("hmac") => hmac_of(data),
            w if w.starts_with("gcm") => {
                let sealed = if w.starts_with("gcm128") {
                    aes_gcm::Aes128Gcm::new_from_slice(&key[..16]).unwrap().encrypt((&nonce).into(), Payload { msg: data, aad: &aad })
                } else {
                    aes_gcm::Aes256Gcm::new_from_slice(&key).unwrap().encrypt((&nonce).into(), Payload { msg: data, aad: &aad })
                }
                .unwrap();
                let (ct, tag) = sealed.split_at(len);
                if w.ends_with("-tag") { hexs(tag) } else if w.ends_with("-tail") { hexs(&ct[ct.len() - 32..]) } else { hexs(&ct[..ct.len().min(32)]) }
            }
            _ => continue,
        };
        checked += 1;
        if got != want {
            bad.push(format!("{what} len {len}: got {got}, want {want}"));
        }
    }
    assert!(log.matches("error").count() == 0 || bad.is_empty(), "{log}");
    assert!(checked >= 60 + 27 + 12 + 32, "only {checked} results checked:\n{log}");
    assert!(bad.is_empty(), "{} of {checked} results differ:\n{}", bad.len(), bad.join("\n"));
}

/// MULTI-01: a NOTE4 and a NOTE4C in one process, interleaved slice by slice, stay independent:
/// separate flash, panel, buttons and battery. Friday v0.1.2 (user's local release,
/// private) is the NOTE4C firmware.
#[test]
#[ignore = "needs the NOTE4 demo fixture and the local Friday v0.1.2 release"]
fn note4_and_note4c_run_side_by_side_independently() {
    let private = private_dir();
    let friday_path = private.join(".device-tools/friday-release/today-is-friday-v0.1.2-note4c-merged-offset-0x0.bin");
    // Reference: Friday alone, same settings.
    let mut solo = boot("note4c", &friday_path);
    solo.board.lock().set_battery_mv(4150);
    run_until(&mut solo, 8.0);
    let solo_image = solo.board.lock().panel.visible().to_vec();

    let mut note4 = note4_demo();
    let mut note4c = boot("note4c", &friday_path);
    note4.board.lock().set_battery_mv(3500);
    note4c.board.lock().set_battery_mv(4150);
    let mut t = 0.0;
    let mut menu = None;
    while t < 8.0 - 1e-9 {
        t += 0.05;
        run_until(&mut note4, t);
        run_until(&mut note4c, t);
        if (t - 3.0).abs() < 0.026 {
            menu = Some(fnv1a(note4.board.lock().panel.visible()));
        }
        if (t - 4.0).abs() < 0.026 {
            note4.set_button("down", true).unwrap();
        }
        if (t - 4.1).abs() < 0.026 {
            note4.set_button("down", false).unwrap();
        }
    }
    let (n4, n4c) = (note4.board.lock(), note4c.board.lock());
    assert_eq!(menu, Some(MENU_HASH), "NOTE4 menu unaffected by the neighbour");
    assert_eq!(n4.panel.refreshes.len(), 3, "NOTE4: splash, menu, then the DOWN move");
    assert_eq!(format!("{:?}", n4.panel.refreshes[2].kind), "Partial");
    assert_eq!(n4c.panel.refreshes.len(), 1, "NOTE4C: its first-setup screen only; NOTE4's button did not reach it");
    assert!(n4c.panel.visible() == &solo_image[..], "NOTE4C panel identical to a solo run");
    assert_eq!((n4.battery_mv, n4c.battery_mv), (3500, 4150));
}

fn run_diag_until(nm: &mut NoteMachine, log: &mut String, t: &mut f64, marker: &str) {
    while !log.contains(marker) {
        *t += 0.25;
        assert!(*t < 120.0, "no {marker:?}:\n{log}");
        run_until(nm, *t);
        log.push_str(&console_text(nm));
    }
}

/// SPI-01 and INPUT-01 on fixtures/diag-fw/io: a 30000-byte SPI3 DMA transaction (chained GDMA
/// descriptors) lands byte-exact in the panel, and IDF-ISR-service edge interrupts count each
/// press of UP and DOWN exactly once.
#[test]
#[ignore]
fn spi3_dma_chain_and_gpio_edge_interrupts() {
    let dir = repo().join(".tools/fixtures/diag-io");
    assert!(dir.join("flasher_args.json").is_file(), "run scripts/build-fixtures.sh diag-fw/io");
    let mut nm = boot("note4c", &dir);
    let (mut log, mut t) = (String::new(), 0.0);
    run_diag_until(&mut nm, &mut log, &mut t, "DIAG gpio ready");
    assert!(log.contains("DIAG spi done"), "{log}");
    {
        let st = nm.board.lock();
        let expected: Vec<u8> = (0..30000u32).map(|i| (i.wrapping_mul(37).wrapping_add(11)) as u8).collect();
        let visible = st.panel.visible();
        assert_eq!(visible.len(), expected.len());
        let first_bad = visible.iter().zip(&expected).position(|(a, b)| a != b);
        assert_eq!(first_bad, None, "panel differs from the DMA source");
        assert!(st.panel.diagnostics.is_empty(), "{:?}", st.panel.diagnostics);
    }
    for (i, button) in ["up", "down", "up", "up", "down"].iter().enumerate() {
        press(&mut nm, button, t + 0.2 + 0.3 * i as f64);
    }
    t += 2.0;
    run_until(&mut nm, t);
    log.push_str(&console_text(&mut nm));
    let last = log.lines().filter(|l| l.starts_with("DIAG gpio up=")).last().unwrap_or("");
    assert_eq!(last, "DIAG gpio up=3 down=2", "{log}");
}

fn diag_lines<'a>(log: &'a str, prefix: &str) -> Vec<&'a str> {
    let mut v: Vec<&str> = log.lines().filter(|l| l.starts_with(prefix)).collect();
    v.dedup(); // twinned on UART0 and USB
    v
}

/// RTC-01 on firmware (fixtures/diag-fw/board): the guest sets 2026-12-31 23:59:58, reads the
/// year rollover back, and a minute alarm pulls INT low through a GPIO5 interrupt; clearing AF
/// releases it. BATT-02: battery and charger changes arrive through the guest's own ADC and
/// GPIO reads, including NOTE4C "full".
#[test]
#[ignore]
fn board_diag_rtc_rollover_alarm_and_battery_states() {
    use note_machine::board::ChargeSense;
    let dir = repo().join(".tools/fixtures/diag-board");
    assert!(dir.join("flasher_args.json").is_file(), "run scripts/build-fixtures.sh diag-fw/board");
    let mut nm = boot("note4c", &dir);
    nm.board.lock().set_battery_mv(3700);
    let (mut log, mut t) = (String::new(), 0.0);
    run_diag_until(&mut nm, &mut log, &mut t, "DIAG rtc armed");
    let times = diag_lines(&log, "DIAG rtc ");
    assert!(times.contains(&"DIAG rtc set 2026-12-31 23:59:58 vl=0"), "{log}");
    assert!(times.iter().any(|l| l.starts_with("DIAG rtc later 2027-01-01 00:00:0")), "{log}");
    let batt = |log: &str| diag_lines(log, "DIAG batt ").last().map(|s| s.to_string()).unwrap_or_default();
    let mv = |line: &str| line.split(['=', ' ']).nth(3).and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
    let first = batt(&log);
    assert!((mv(&first) - 3700).abs() <= 40 && first.ends_with("chg=1 full=0"), "{first}");

    assert_eq!(nm.set_charger(true, None), ChargeSense::Charging);
    nm.board.lock().set_battery_mv(4050);
    t += 1.0;
    run_until(&mut nm, t);
    log.push_str(&console_text(&mut nm));
    let charging = batt(&log);
    assert!((mv(&charging) - 4050).abs() <= 40 && charging.ends_with("chg=0 full=0"), "{charging}");

    assert_eq!(nm.set_charger(false, Some(true)), ChargeSense::Full);
    t += 1.0;
    run_until(&mut nm, t);
    log.push_str(&console_text(&mut nm));
    assert!(batt(&log).ends_with("chg=1 full=1"), "{}", batt(&log));

    run_diag_until(&mut nm, &mut log, &mut t, "DIAG rtc cleared");
    let rtc = diag_lines(&log, "DIAG rtc ");
    assert!(rtc.iter().any(|l| l.starts_with("DIAG rtc alarm 2027-01-01 00:01:0")), "{log}");
    assert!(rtc.contains(&"DIAG rtc af=1 int=0") && rtc.contains(&"DIAG rtc cleared int=1"), "{log}");
}

/// Row-level summary of a gray4 image: per row, Some(level) if uniform, None otherwise.
fn gray_rows(visible: &[u8]) -> Vec<Option<u8>> {
    visible
        .chunks(200)
        .map(|row| {
            let l = row[0] >> 4;
            row.iter().all(|&b| b == (l << 4 | l)).then_some(l)
        })
        .collect()
}

/// DISP-03 on firmware (fixtures/diag-fw/panel, NOTE4): full, short-frame, unknown-waveform,
/// reset-during-refresh and unpowered paths leave the documented image, BUSY and diagnostics.
#[test]
#[ignore]
fn panel_diag_edge_paths_have_documented_state() {
    use note_machine::ssd2683::Diagnostic;
    let dir = repo().join(".tools/fixtures/diag-panel");
    assert!(dir.join("flasher_args.json").is_file(), "run scripts/build-fixtures.sh diag-fw/panel");
    let mut nm = boot("note4", &dir);
    let (mut log, mut t) = (String::new(), 0.0);
    let state = |nm: &mut NoteMachine, log: &mut String, t: &mut f64, name: &str| {
        run_diag_until(nm, log, t, &format!("DIAG step {name} "));
        let line = log.lines().find(|l| l.starts_with(&format!("DIAG step {name} "))).unwrap().to_string();
        let busy: i64 = line.rsplit('=').next().unwrap().parse().unwrap();
        let st = nm.board.lock();
        (busy, gray_rows(st.panel.visible()), st.panel.refreshes.len(), st.panel.diagnostics.clone())
    };
    let stripes: Vec<Option<u8>> = (0..300).map(|y| Some(if y % 2 == 0 { 15 } else { 0 })).collect();

    let (busy, rows, n, diags) = state(&mut nm, &mut log, &mut t, "full");
    assert_eq!((rows == stripes, n, diags.len()), (true, 1, 0), "{diags:?}");
    assert!((140_000..=170_000).contains(&busy), "full refresh BUSY {busy} us (fast timing: 150 ms)");

    // Short frame: RAM keeps what the rest of the previous frame wrote.
    let (_, rows, n, diags) = state(&mut nm, &mut log, &mut t, "short");
    let mut short = stripes.clone();
    short[..10].fill(Some(0));
    assert_eq!((rows == short, n, diags.len()), (true, 2, 0), "{diags:?}");

    // Unknown external waveform: diagnostic, image kept, BUSY still runs.
    let (busy, rows, n, diags) = state(&mut nm, &mut log, &mut t, "unknown");
    assert_eq!((rows == short, n), (true, 2));
    assert_eq!(diags, [Diagnostic::UnknownPanelWaveform { lut_len: 535 }]);
    assert!((140_000..=170_000).contains(&busy), "{busy}");

    // Reset 20 ms into a refresh: BUSY was low, reset ends the operation; the refresh had
    // already committed the image (the model commits at 0x12, not at the end of BUSY).
    let (busy, rows, n, _) = state(&mut nm, &mut log, &mut t, "reset");
    assert!(log.contains("DIAG reset busy_before=0"), "{log}");
    assert!(busy < 20_000, "BUSY after reset {busy} us");
    assert_eq!((rows.iter().all(|r| *r == Some(15)), n), (true, 3));

    // Traffic with the rail off is reported and ignored; the image survives power off.
    let (_, rows, n, diags) = state(&mut nm, &mut log, &mut t, "unpowered");
    assert_eq!((rows.iter().all(|r| *r == Some(15)), n), (true, 3));
    assert_eq!(&diags[1..], [Diagnostic::UnpoweredTraffic { bytes: 1 }, Diagnostic::UnpoweredTraffic { bytes: 10 }]);

    let (busy, rows, n, diags) = state(&mut nm, &mut log, &mut t, "recover");
    assert_eq!((rows == stripes, n, diags.len()), (true, 4, 3));
    assert!((140_000..=170_000).contains(&busy), "{busy}");
}

/// Console text, panel pixels and counters from `from` to `to` seconds.
fn continue_run(nm: &mut NoteMachine, to: f64) -> (String, u32, usize, u64) {
    let mut log = String::new();
    let mut t = nm.seconds();
    while t < to {
        t = (t + 0.25).min(to);
        run_until(nm, t);
        log.push_str(&console_text(nm));
    }
    let st = nm.board.lock();
    (log, fnv1a(st.panel.visible()), st.panel.refreshes.len(), nm.cycles())
}

/// SNAP-01/02 on firmware: a snapshot taken at `save_at` and restored into a different
/// machine (same profile and firmware, elsewhere in its run) continues exactly like the
/// uninterrupted run: identical console bytes, panel pixels, refresh count and cycle count.
fn snapshot_continues_identically(profile: &str, firmware: &Path, save_at: f64, until: f64) -> (String, u32) {
    let hash = [7u8; 32];
    let mut a = boot(profile, firmware);
    run_until(&mut a, save_at);
    let _ = console_text(&mut a);
    let snap = a.save_snapshot(hash).expect("save");
    let straight = continue_run(&mut a, until);

    let mut b = boot(profile, firmware);
    run_until(&mut b, save_at / 3.0 + 0.1);
    let _ = console_text(&mut b);
    let report = b.restore_snapshot(&snap, hash).expect("restore");
    assert!(!report.partial);
    assert_eq!(b.seconds(), save_at, "virtual time restored");
    let restored = continue_run(&mut b, until);
    assert_eq!(restored.0, straight.0, "{profile}: console after restore differs");
    assert_eq!((restored.1, restored.2, restored.3), (straight.1, straight.2, straight.3), "{profile}: panel/refreshes/cycles");
    (straight.0, straight.1)
}

#[test]
#[ignore]
fn snapshot_restores_the_note4_demo_mid_boot_bit_exact() {
    let fw = repo().join("third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin");
    // 1.2 s is between the splash and the menu: the panel, SPI3 DMA and BUSY are all live.
    let (_, pixels) = snapshot_continues_identically("note4", &fw, 1.2, 3.0);
    assert_eq!(pixels, MENU_HASH, "the restored machine reaches the same menu");
}

#[test]
#[ignore]
fn snapshot_restores_note4c_rtc_adc_and_i2c_state_bit_exact() {
    let dir = repo().join(".tools/fixtures/diag-board");
    // 2.0 s: the RTC was set and is counting towards the alarm; ADC and GPIO IRQs are armed.
    let (log, _) = snapshot_continues_identically("note4c", &dir, 2.0, 70.0);
    assert!(log.contains("DIAG rtc alarm 2027-01-01 00:01:0"), "{log}");
}

/// FW-05 (legacy): Friday's `HOME_EMULATOR` build draws over the console, not the panel. The
/// decoded console frame is the display, frame rows are gone from the console, and it is the
/// same image the native release draws on the panel. Both are the user's local Friday builds.
#[test]
#[ignore]
fn legacy_friday_console_frame_matches_the_native_panel() {
    let private = private_dir();
    let legacy_dir = private.join(".device-tools/friday-source/build-emulator");
    let native = private.join(".device-tools/friday-release/today-is-friday-v0.1.2-note4c-merged-offset-0x0.bin");
    let mut legacy = boot("note4c", &legacy_dir);
    run_until(&mut legacy, 3.0);
    let log = console_text(&mut legacy);
    assert!(!log.contains("BEGIN_FRAME") && !log.contains("VVVVVVVV"), "frame rows left the console");
    assert_eq!(legacy.display_source(), 2, "legacy console frame");
    assert!(legacy.board.lock().panel.refreshes.is_empty(), "the legacy build never drives the panel");
    let mut panel = boot("note4c", &native);
    run_until(&mut panel, 3.0);
    let _ = console_text(&mut panel);
    assert_eq!(panel.display_source(), 1);
    assert_eq!(fnv1a(&legacy.display_frame()), fnv1a(&panel.display_frame()));
    // A snapshot of the legacy build keeps its console-drawn display.
    let snap = legacy.save_snapshot([3; 32]).unwrap();
    let mut other = boot("note4c", &legacy_dir);
    run_until(&mut other, 0.05);
    other.restore_snapshot(&snap, [3; 32]).unwrap();
    assert_eq!((other.display_source(), fnv1a(&other.display_frame())), (2, fnv1a(&legacy.display_frame())));
}

/// FW-04 display/battery and a level-interrupt regression: native emini arms its three buttons
/// as LOW_LEVEL interrupts (wake sources). Before PATCHES #19 the IDF ISR service never found
/// the pin and core 0 stormed while a key was held. After the five-minute pairing window a
/// short OK opens the info card, which draws the battery the guest measured (3500 mV → 11 %
/// by emini's curve). User's local emini build.
#[test]
#[ignore]
fn emini_buttons_work_and_the_info_card_follows_the_battery() {
    let private = private_dir();
    let emini = private.join(".device-tools/emini-home/firmware/build");
    let mut nm = boot("note4c", &emini);
    nm.board.lock().set_battery_mv(3500);
    run_until(&mut nm, 305.0);
    let before = nm.board.lock().panel.refreshes.len();
    let _ = console_text(&mut nm);
    press(&mut nm, "ok", 310.0);
    run_until(&mut nm, 316.0);
    let log = console_text(&mut nm);
    assert!(log.contains("KEY key=4") && log.contains("action=press"), "{log}");
    assert_eq!(nm.board.lock().panel.refreshes.len(), before + 1, "the info card is drawn");
}

/// Light and deep sleep on unmodified ESP-IDF code (fixtures/diag-fw/sleep, PATCHES #18/#20):
/// a 500 ms timer light sleep wakes as TIMER after ~0.5 s of esp_timer time; a GPIO light
/// sleep (UP low) wakes as GPIO when the key is pressed, not before (the RTC WDT safety net
/// pauses in sleep); two 1 s deep sleeps reboot with cause DEEPSLEEP, keep RTC memory, and
/// report TIMER as the wake-up cause.
#[test]
#[ignore]
fn light_and_deep_sleep_wake_on_timer_and_gpio() {
    let dir = repo().join(".tools/fixtures/diag-sleep");
    assert!(dir.join("flasher_args.json").is_file(), "run scripts/build-fixtures.sh diag-fw/sleep");
    let mut nm = boot("note4c", &dir);
    let mut log = String::new();
    // Deep sleep ends in a reboot: keep running through it.
    let run_to = |nm: &mut NoteMachine, log: &mut String, until: f64, marker: &str| {
        let mut t = nm.seconds();
        while !log.contains(marker) && t < until {
            t += 0.25;
            while nm.run_until((t * CPU_HZ as f64) as u64) != SliceEnd::Reached {}
            log.push_str(&console_text(nm));
        }
        assert!(log.contains(marker), "no {marker:?}: {log}");
    };
    run_to(&mut nm, &mut log, 2.9, "DIAG waiting for UP");
    let slept: i64 = log.split("slept_ms=").nth(1).and_then(|r| r.split_whitespace().next()).and_then(|v| v.parse().ok()).unwrap_or(-1);
    assert!(log.contains("DIAG light timer cause=4") && (450..=510).contains(&slept), "{log}");
    run_until(&mut nm, 3.0);
    nm.set_button("up", true).unwrap();
    run_until(&mut nm, 3.2);
    nm.set_button("up", false).unwrap();
    log.push_str(&console_text(&mut nm));
    run_to(&mut nm, &mut log, 10.0, "DIAG done");
    let at: i64 = log.split("at_ms=").nth(1).and_then(|r| r.split_whitespace().next()).and_then(|v| v.parse().ok()).unwrap_or(-1);
    assert!(log.contains("DIAG light gpio cause=7") && (2900..=3100).contains(&at), "woke at the press: {log}");
    assert!(log.contains("DIAG boot n=2 reset=8 cause=4") && log.contains("DIAG boot n=3 reset=8 cause=4"), "{log}");
    assert!(!log.contains("RTCWDT"), "{log}");
}
