# IDF hello_world for the ESP32-C3

The ESP-IDF 5.5 `get-started/hello_world` example, built with `idf.py set-target esp32c3 build`.
The end-to-end test for the RISC-V model: mask ROM → 2nd-stage bootloader → FreeRTOS → `app_main`
→ `esp_restart()` → round again.

    ../../target/release/esp32sim-c3 --boot rom --flash-mb 4 \
      --bootloader build/bootloader/bootloader.bin --ptable build/partition_table/partition-table.bin \
      --app build/hello_world.bin --elf build/hello_world.elf --max-seconds 26

Expect three complete boots in 26 emulated seconds: `Hello world!`, the ten-second countdown, and
`rst:0x1 (POWERON)` again after each `esp_restart()`. See ../../docs/esp32c3.md.
