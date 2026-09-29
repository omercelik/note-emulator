use emu_core::{Bus, Fault};
use esp32c3::bus::{SocBus, RTC_SLOW_HIGH};

fn bus() -> SocBus { SocBus::new(1, [0; 6]) }

#[test]
fn reads_ending_at_buffer_boundary_use_their_full_width() {
    let mut b = bus();
    let end = b.rtc_slow.len();
    b.rtc_slow[end - 4..].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
    assert_eq!(b.read8(RTC_SLOW_HIGH - 1), Ok(0x44));
    assert_eq!(b.read16(RTC_SLOW_HIGH - 2), Ok(0x4433));
    assert_eq!(b.read32(RTC_SLOW_HIGH - 4), Ok(0x4433_2211));
}

#[test]
fn reads_crossing_buffer_boundary_are_unmapped() {
    let mut b = bus();
    assert_eq!(b.read16(RTC_SLOW_HIGH - 1), Err(Fault::Unmapped));
    assert_eq!(b.read32(RTC_SLOW_HIGH - 3), Err(Fault::Unmapped));
}
