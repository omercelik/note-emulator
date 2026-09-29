# Waveshare ESP32-S3-Touch-LCD-4B · esp32-screen energy panel in the emulator

    ./run-energy-panel.sh --web 8768      # http://127.0.0.1:8768/ — 480x480 panel, click/drag to touch
    ./run-energy-panel.sh --script touch.txt --tft-png panel.png --max-seconds 6
    ./run-energy-panel.sh --script sid.txt --wav sid.wav --max-seconds 25   # SID page, press play
    ./run-energy-panel.sh --wifi "ssid=NAME,psk=PASS" --web 8768    # live data

What works: ST7701S init through the TCA9554, LVGL UI on the RGB bus at 60 fps, GT911 touch (taps,
swipes between the tileview pages), the SID player (libcRSID → ES8311 → I2S0, audible in the UI or
`--wav`). Check audio without listening: `hw/wsaudio.py 8768 8`. `sid.txt` is the scripted
version: three swipes to the SID tile, then the play button — the capture is silent until the
tap and then carries the tune, and the whole thing runs at about real time.

**With `--wifi`** the panel runs its real network stack on the unmodified WiFi blob: it associates
with the emulated AP, takes a DHCP lease, syncs its clock, fetches electricity prices over HTTPS and
polls Home Assistant, so the price chart, the energy history and the control tiles fill in with live
data. The SSID and passphrase must be the ones the firmware was built for, and the Home Assistant
instance it points at has to be reachable from this machine. See
[../../docs/networking-howto.md](../../docs/networking-howto.md).

With `--flash-at 0x610000=../../web/wasm/fw/public/energydata.json` the firmware boots in **demo mode**
(canned prices, energy bars and tile states from that file, fixed clock, WiFi never started) — the
configuration the public WebAssembly page uses. Without `--wifi` (and without demo data) the script stubs `esp_wifi_start`, because with no network to join the blob spins
in PHY calibration on core 0 and starves the LVGL task; prices and Home Assistant tiles then stay
empty, which is the right way to demo the UI offline.
