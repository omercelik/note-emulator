# emini Home — NOTE4C sample

English-first weather, headlines, and notes firmware for NOTE4C, from [fiedoruk/emini-home v0.6.2](https://github.com/fiedoruk/emini-home/releases/tag/v0.6.2). Source for the application is available at that tag.

The upstream application and partition table are unmodified and verified against upstream SHA256SUMS. The merged image combines an ESP-IDF 6.0 bootloader built from this repository's fixtures/diag-fw/panel, the upstream partition table at 0x8000, and the application at 0x20000. All other flash bytes are erased. merged.sha256 checks the combined image.

Bootloader source/build: scripts/build-fixtures.sh diag-fw/panel (ESP-IDF 6.0). ESP-IDF source: https://github.com/espressif/esp-idf/tree/v6.0.

Own firmware: MIT. See LICENSE and THIRD_PARTY_NOTICES.md. All referenced component and font notices are included here. Embedded Mozilla root-certificate source is available from https://github.com/espressif/esp-idf/blob/v6.0/components/mbedtls/esp_crt_bundle/cacrt_all.pem (MPL-2.0).

The default language is English. Use Setup networking so the original http://192.168.4.1 address meets the firmware's local-origin checks. On first boot, enter the hotspot password shown on the display in emulator Controls → Network, then use the browser setup URL and pairing code. This is community firmware, not stock factory firmware.
