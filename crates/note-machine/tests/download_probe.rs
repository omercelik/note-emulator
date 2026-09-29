//! ROM download mode over USB-Serial/JTAG (DEV-02): after an EN reset with GPIO0 low, the mask
//! ROM answers esptool's SYNC command. NOTE boards are flashed over the S3's native USB.
use std::path::Path;

use note_core::{flash, profile, rom};
use note_machine::{NoteMachine, RadioConfig, CPU_HZ};

#[test]
#[ignore = "needs the ROM and third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin"]
fn rom_download_mode_answers_esptool_sync_over_usb_serial_jtag() {
    let p = profile::find(&profile::profiles_dir(), "note4").unwrap();
    let rom = std::fs::read(rom::installed().unwrap().path).unwrap();
    let fw = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin");
    let image = flash::load(&fw, 16 << 20).unwrap();
    let mut nm = NoteMachine::new(&p, &rom, &image, [2, 0, 0, 0, 0, 4], 0, &RadioConfig::default()).unwrap();
    nm.pin_reset(true);
    nm.run_until(CPU_HZ / 5);
    let banner = String::from_utf8_lossy(&nm.take_console().uart0).into_owned();
    assert!(banner.contains("boot:0x0 (DOWNLOAD(USB/UART0))") && banner.contains("waiting for download"), "{banner}");
    // SLIP frame: direction 0, SYNC (0x08), 36-byte payload 07 07 12 20 + 32 x 55, checksum 0.
    let mut sync = vec![0xc0, 0x00, 0x08, 0x24, 0x00, 0, 0, 0, 0, 0x07, 0x07, 0x12, 0x20];
    sync.extend([0x55; 32]);
    sync.push(0xc0);
    nm.serial_input(&sync);
    nm.run_until(CPU_HZ / 5 * 2);
    let reply = nm.take_console().usb;
    // A response frame: c0, direction 1, command 0x08.
    assert!(reply.windows(3).any(|w| w == [0xc0, 0x01, 0x08]), "no SYNC response: {:02x?}", &reply[..reply.len().min(48)]);
}
