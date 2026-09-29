# Waveshare ESP32-C6-LCD-1.47 — the IEEE 802.15.4 energy scanner

The board the ESP32-C6 model was brought up on, running its owner's firmware: an IEEE 802.15.4
energy scanner with an LVGL spectrum on the 172×320 ST7789 (github.com/joakimeriksson/esp32,
`energy_scan`, ESP-IDF, `idf.py set-target esp32c6 build`). The sources are not in this
repository; point `ENERGY_SCAN_DIR` at the project and run:

    ENERGY_SCAN_DIR=~/work/esp32/energy_scan examples/waveshare-c6-lcd147/run.sh --max-seconds 8 --tft-png lcd.png

What runs: the real ROM and bootloader, FreeRTOS, `nvs_flash_init` (the PHY stores calibration
data in flash), the WS2812 through the RMT, the ST7789 through SPI2 and the GDMA, LVGL at 2 ms
ticks, the PHY and BTBB blobs' init, and `esp_ieee802154_energy_detect` sweeping channels 11–26
through the emulated 802.15.4 MAC, which answers each scan with a synthetic 2.4 GHz picture (a
quiet floor with the three WiFi channels on top). The end-of-run report counts scans, SPI
transfers, RMT frames and display writes; `--tft-png` writes what the panel shows; `press boot 150`
in a script presses the BOOT button (GPIO 9).

`--stub bb_init=0` skips the PHY blob's baseband calibration (TX DC, IQ, power detector — hardware
handshakes on undocumented registers that the model does not have). Everything else in the PHY
init runs; the 802.15.4 MAC model does not depend on the PHY. See ../../docs/esp32c6.md.
