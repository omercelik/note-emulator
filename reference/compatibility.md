# Compatibility report (living)

Results are per firmware hash and workflow: `unverified` · `passed` · `failed` · `unsupported`.
A boot result never implies other workflows work. Fixture ids name the firmware image used.
Engine: esp32sim 4ab7e90 + PATCHES.md #1–#24 (as of 2026-09-29). Rows were added as each gate ran; the evidence column names the run, and later rows supersede earlier "unverified" ones.

## Workflows (2026-09-27, G1)

| Fixture | Workflow | Status | Evidence |
|---|---|---|---|
| note4-reference-demo | boot ROM → app, board init (RTC ready, NFC absent) | passed | console `zectrix_board: board initialized rtc=1 nfc=0` |
| note4-reference-demo | full 1bpp refresh over SPI3 + DMA, temperature readback | passed | splash + menu frames; 4 readbacks per 5 refreshes (partial path reads none) |
| note4-reference-demo | partial refresh, exterior preserved | passed | DOWN/DOWN/UP → `Partial rect=(0,36,400,234)`, header/footer intact |
| note4-reference-demo | buttons: press, long hold | passed | menu navigation; `Hold OK Back` (1.6 s) returns to menu |
| note4-reference-demo | battery voltage via ADC + eFuse calibration (BATT-01) | passed | configured 3500/3900/4150 mV → DEVICE INFO 3500/3898/4150 mV |
| note4-reference-demo | power test (PWR tab) | passed | `PWR PASS` |
| note4-reference-demo | 16-gray 4bpp refresh (DISP-02) | passed | Display Gallery → 16-GRAY: five recognised passes, all 16 levels, no diagnostics; pixel hash pinned in `note4_demo_draws_the_16_gray_gallery_scene` |
| note4-reference-demo | long-hold DOWN shutdown, cold wake (POWER-01) | passed | `clearing display before shutdown`, `cutting battery latch` → power off, image kept; wake prints `rst:0x1 (POWERON)` and redraws (`note4_demo_long_hold_down_powers_off_and_wakes_cold`) |
| note4c-factory | boot, board init, four-colour frame over real SPI3 | passed | factory test screen; palette histogram equals the old QEMU capture exactly (white 62360, yellow 26502, black 24216, red 6922) |
| note4c-factory | factory RF test against the virtual AP | passed | `factory_test type=rf ssid=esp32sim rssi=-40 threshold=>-50`, state 2 |
| note4c-factory | factory audio test | passed | I²S RX + speaker→mic loopback (PATCHES #12): `type=audio result=PASS fc=3000` |
| note4c-factory | factory RTC and charging tests | passed | RTC `state=2`; charging `state=2` after `--usb-at` plugs USB |
| note4c-factory | factory NFC test | failed (unsupported) | NFC not modelled: `NFC 设备未初始化`; the flow still reaches its result screen |
| note4c-factory | documented button action (FW-02) | passed | result screen `长按确认键关机`: long-press OK → firmware opens the latch → board power off (`note4c_factory_flow_and_long_press_ok_powers_off`) |
| emini-home-native | boot, SoftAP 192.168.4.1, setup screen over interrupt-driven SPI | passed | setup/pairing frame; `DHCP server started on interface WIFI_AP_DEF with IP: 192.168.4.1` |
| emini-home-native | browser access to 192.168.4.1 | passed | the Connect button applies the hotspot password and checks the page; the setup page opened in a Mac browser through the installed helper (2026-09-30) |
| emini-home-native | its station page at `http://10.0.2.15/` after setup (ADR-019) | passed | the helper's station lease: `note-emu` listens on `10.0.2.15:80`, `GET /` → 200, and the page opened in a Mac browser (2026-09-30) |
| emini-home-native | buttons; info card with the guest-measured battery (FW-04 display/battery) | passed | after the pairing window, a short OK opens the info card: 3900 mV → **76 %**, 3500 mV → **11 %** (emini's own curve, via ADC + eFuse calibration); `KEY key=4 action=press`. Needed PATCHES #19 (level interrupts in GPIO STATUS). Before that fix the buttons never worked and core 0 stormed while a key was held (`emini_buttons_work_and_the_info_card_follows_the_battery`) |
| emini-home-native (paired, battery, no USB host) | PM automatic light sleep between events; button wake | passed | `--usb-host off`: locks released after the pairing window, 488 light sleeps in 100 s, OK and UP each wake the chip and redraw within 0.8 s (info card, feed); 400 s in 42 s wall (PATCHES #18–#21). Also fixed: `note-emu --avd --press` stopped the run at the first press |
| emini-home-native (AVD) | provisioning survives reboot; AP+STA → STA (NET-05) | passed | pair + note over the SoftAP relay, `ndb stop`, cold boot: still paired, note revision kept, the note screen returns after the pairing window; `POST /api/wifi` joins a WPA2 virtual `HomeNet` while the AP stays (APSTA); reboot comes up `mode : sta`, rejoins from NVS and draws a live feed over the NAT (see g2b-handoff) |
| friday-native-release | boot, first-time setup screen (SoftAP) | passed | `首次设置` frame |
| friday-native-release | local provisioning over its open AP (FW-05 native) | passed | `--softap --nat`: the peer joins the open `今天是周五吗-…` AP; the portal's `/scan` lists `esp32sim`; `POST /submit` (four fields, as the page sends) → `success, sntp_synced`; the guest joins `esp32sim` (10.0.2.15); `POST /exit?token=…` closes the AP and it draws the day face ("今天是周五吗？ 不是。", correct date and weekday from the submitted time; PNG sha256 943dd5a73d9a…) |
| friday-native-release | real Mac browser at `http://192.168.4.1/` (NET-02) | passed | installed root helper supplied `192.168.4.1:80`; browser selected `esp32sim`, submitted the form, reached the success page; native panel changed from first setup to day face (two distinct frame hashes; 2026-09-28) |
| friday-native-release | buttons | passed | OK → `BOOT click forwarded`, UP/DOWN click logged as unused by design, DOWN hold → `DOWN long press forwarded`; in setup these are policy no-ops (not a model fault) |
| friday-legacy-build (`build-emulator`, local) | legacy console display (FW-05 legacy, preview path D8) | passed | `BEGIN_FRAME` blocks are decoded as the display (protocol `source` 2) and removed from the console; the first-setup frame is byte-identical to the native release's panel frame (`legacy_friday_console_frame_matches_the_native_panel`). OK is forwarded; the setup card waits for provisioning like the native build |
| geminilive (local build f65751f, secrets compiled in; never committed) | boot, home Wi-Fi join, TLS, Home screen | passed | 8 MB octal PSRAM found; `--wifi-env` gives the virtual AP the project `.env`'s SSID/passphrase (WPA2); DHCP 10.0.2.15; `Certificate validated`; Open-Meteo date/weather drawn (four-colour Home frame) |
| geminilive | push-to-talk turn with Gemini Live (WAV mic) | passed | OK held, `ndb mic` question WAV → `Gemini Live session ready`, `Talk turn sent`, spoken reply (`audio 2.2s`) captured on the speaker |
| geminilive | card tool → e-paper | passed | "Pin three short facts about Paris" → `Gemini card accepted ("Facts About Paris")`, panel redraw with the card |
| geminilive | live mic stream (`audio.mic` pcm, as the app's Mac-microphone toggle sends it) | passed | 60 ms base64 chunks while OK is held → full turn and reply |
| geminilive | reply audio without underruns | passed | before PATCHES #23 the NAT stalled 300 ms per guest-side loss (22–43 underruns per turn); with fast retransmit 0 (hardware logs about 1) |
| geminilive | quick boot (new process) then talk | passed | restored at t=435 s; the AP deauthenticates the restored station (PATCHES #24), it rejoins and reports `Wi-Fi link restored`; the first press can hit the firmware's own DNS race right at link-up, the next completes (Google's `1011` on setup is retried by the firmware). The first reply after the rejoin stuttered (47 underruns); the next three added 0 |
| note4c-factory | status LED (GPIO3) | passed | blinks about twice a second through the run (vendor `PowerLedTask`) |
| geminilive | status LED while talking | passed | off at idle; lit exactly while OK is held to talk (`controls.rs`), matching hardware |

## Product track on replay (2026-09-27, G3)

AVD storage, log merge, the 64-byte frame header, and the protocol session are tested
against `ReplayMachine`. `ndb avd create|list|wipe|delete|import`
and `ndb -s ID logcat` are in. `note-emu --avd` boots the stored flash and serves that
socket on the emulation thread. A release test on the NOTE4 demo checks a full 400×300
frame and a non-empty console through the same session.

The SwiftUI device window uses that socket. Its model, on a
release build, reaches the NOTE4 demo menu and changes it with Down, and it shows a
four-colour NOTE4C factory frame. Battery, the log, a live forward URL, focus-loss
release, and a second window's full frame are covered by `DeviceWindowLiveTests`.

## Host networking (2026-09-27, G2a)

Inbound TCP is implemented in the NAT (`PATCHES.md` #6) and reachable with `note-emu --forward`
(loopback) or `--setup-address` (helper-owned `192.168.4.1:80`). No unmodified firmware has been
asked to serve HTTP or complete TLS through it yet, so NET-01 and FLOW-02 stay unverified at the
firmware layer. Disabled networking (`--nat` off and no forward) still opens no host socket.
Multicast/mDNS remains unsupported. 

## Performance baseline (reference Mac, 2026-09-27, 10 s virtual per run)

| Firmware | JIT: first frame (wall) / speed | Interpreter: first frame (wall) / speed |
|---|---|---|
| NOTE4 demo | 0.18 s / 11.1× real time | 0.28 s / 9.8× |
| NOTE4C factory | 0.25 s / 6.7× | 0.39 s / 5.4× |
| emini native | 0.52 s / 7.6× | 0.75 s / 6.5× |
| Friday v0.1.2 | 0.23 s / 9.4× | 0.30 s / 8.8× |

First-frame virtual times are identical with and without the JIT (0.225 / 0.226 / 0.664 / 0.486 s).
Busy phases run at 140–200 Minsn/s; idle firmware sleeps in `waiti`, so wall speed is dominated
by the scheduler, not the CPU model.

## Baseline — stock esp32sim, `--board none` (G0, before any NOTE work)

| Fixture | Furthest point | Stops because |
|---|---|---|
| note4-reference-demo | `app_main` | no SPI3 / panel; (found in G1: no ADC → spins in `adc_oneshot_read`) |
| note4c-factory | board init | PCF8563 @0x51 absent → abort → reboot loop |

## Station firmware (G2a)

| Fixture | Workflow | Status | Evidence |
|---|---|---|---|
| idf-wifi-station | join virtual AP, DHCP, rejoin after deauth (NET-04 link part) | passed | `station_joins_then_rejoins_after_deauth` |
| idf-http-server | Mac HTTP client → loopback forward → guest port 80, outbound NAT off (NET-01) | passed | `GET /hello` → `200 OK` + `Hello World!`; `POST /echo` echoes the body (chunked); `station_http_server_answers_through_a_loopback_forward` |
| idf-https-request | TLS 1.3 to www.howsmyssl.com with real certificate checks, `--nat --realtime` (FLOW-02) | passed | SNTP, then 3/3 standard requests (crt bundle, cacert_buf, global CA store): `Connection established` ≈ 0.5 s virtual after start, `HTTP/1.1 200 OK`, `"tls_version":"TLS 1.3"`; 60 s virtual in 60.02 s wall; no firmware timeouts changed |
| idf-https-request | forced suites `AES_256_GCM_SHA384` + `AES_128_CCM_SHA256` | open question | server sends a fatal alert (`-0x7780`). The server does negotiate `TLS_AES_256_GCM_SHA384` with `openssl s_client`. Accelerators are verified (next row), including what TLS 1.3 does with them — interleaved and cloned SHA-384 transcript hashes, HMAC-SHA-384 (HKDF) and AES-256-GCM decryption with tag checks — so an emulator crypto fault is ruled out as far as the diagnostics reach; not reproducible against a controlled server because the example hard-codes the host. The deliberately unsupported-suite request fails as the example intends. |
| diag-crypto | SHA-256/384/512 (17 lengths across block/DMA edges; interleaved incremental and cloned contexts), HMAC-SHA-256/384/512, AES-128/256-GCM (0–4096 B) encrypt, decrypt round trip and forged-tag rejection, through PSA + S3 accelerators | passed | all 131 results equal RustCrypto (`crypto_accelerators_match_reference_implementations`) |
| diag-io | one 30000-byte SPI3 transaction through chained GDMA descriptors (SPI-01) | passed | panel RAM byte-identical to the source pattern after refresh, no panel diagnostics (`spi3_dma_chain_and_gpio_edge_interrupts`) |
| diag-io | negative-edge GPIO interrupts via the IDF ISR service on UP (GPIO39) and DOWN (GPIO18) (INPUT-01) | passed | 5 presses → `up=3 down=2`, each counted once |
| diag-board | PCF8563 set to 2026-12-31 23:59:58 by the guest, year rollover, minute alarm → INT (GPIO5) interrupt, AF clear releases INT (RTC-01, fixed clock) | passed | `later 2027-01-01 00:00:01`, `alarm 2027-01-01 00:01:00`, `af=1 int=0` then `cleared int=1` (`board_diag_rtc_rollover_alarm_and_battery_states`) |
| diag-board | battery and charger through guest ADC/GPIO reads: discharging, charging, full (BATT-02, NOTE4C) | passed | 3700 mV → `mv=3700 chg=1 full=0`; charging 4050 mV → `mv=4048 chg=0 full=0`; full → `chg=1 full=1`. NOTE4 full stays `Unknown` (polarity unverified, model test) |
| diag-panel | SSD2683 edge paths on NOTE4 (DISP-03 native) | passed | guest-measured BUSY ≈150 ms per full refresh (fast timing); short frame updates only its 10 rows, the rest of RAM keeps the previous frame; unknown 535-byte LUT → `UnknownPanelWaveform`, image kept, BUSY still runs; RST 20 ms into a refresh ends BUSY within 20 ms, the image is the one committed at `0x12` (model commits at refresh start, not at the end); rail-off traffic → `UnpoweredTraffic` ×2, image retained; re-init after power-up draws normally (`panel_diag_edge_paths_have_documented_state`) |
| note4-reference-demo | full snapshot mid-boot, restored into another machine (SNAP-01/02) | passed | console, pixels, refreshes and cycles identical to the uninterrupted run (`snapshot_restores_the_note4_demo_mid_boot_bit_exact`); quick boot resumes at the menu (SNAP-05) |
| diag-board | full snapshot with RTC/ADC/I2C live, restored (SNAP-01/02); quick boot (SNAP-05) | passed | bit-exact to 70 s incl. the RTC alarm; quick boot resumes mid-delay without a reboot; missing/changed-flash/rejected images cold-boot with the reason |
| diag-sleep | ESP-IDF light sleep (timer, GPIO wake) and deep sleep (timer, RTC memory) | passed | timer: cause TIMER after 491 ms of esp_timer; GPIO: cause GPIO at the UP press (2989 ms), no RTC WDT reset; deep: `rst:0x5 (DSLEEP)`, reset reason DEEPSLEEP, RTC counter kept, cause TIMER (`light_and_deep_sleep_wake_on_timer_and_gpio`) |
| note4-reference-demo + friday-native-release | NOTE4 and NOTE4C in one process, interleaved (MULTI-01) | passed | NOTE4 menu hash and partial move unchanged; NOTE4C panel byte-identical to a solo run; buttons and batteries independent (`note4_and_note4c_run_side_by_side_independently`) |
