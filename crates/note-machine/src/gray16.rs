//! NOTE4 16-level gray passes.
//!
//! The v1.0.0 demo stores five 535-byte external waveforms (`0x20`) back to back.
//! They are the MIT driver's `kVendorGray16RenderWaveforms`: each pass copies whole
//! records out of the vendor four-gray table (`kVendorGray4Waveform`, FNV-1a
//! `0x54F54E58`) so codes 1..3 paint three levels and code 0 holds. Level 15 is
//! never driven; it stays the OTP white base. Level n (0..14) is pass n/3, code
//! (n%3)+1. An 8-bit preview of a level is `level * 17` (0 and 15 map to 0 and 255).
//!
//! Built from the published MIT header's record-copy rules, not from the GPL tree.
//! Any other LUT stays `UnknownPanelWaveform` and does not paint.

use std::sync::OnceLock;

const WAVEFORM: usize = 535;
const ANALOG: usize = 7;
const TABLE: usize = 88;
const RECORD: usize = 11;
const RECORDS: usize = 8;
pub const PASSES: usize = 5;

/// Vendor four-gray table. Analog prefix `00 14 78 78 14 85 08`, then six 88-byte tables.
const VENDOR: [u8; WAVEFORM] = *include_bytes!("gray16_vendor.bin");

#[derive(Clone, Copy)]
struct Spec {
    base: usize,
    alt: usize,
    mask: u8,
}

const HOLD: Spec = Spec {
    base: 5,
    alt: 5,
    mask: 0,
};

/// `kVendorGray16Targets` for levels 0..14. Level 15 is not in the list.
const TARGETS: [Spec; 15] = [
    Spec {
        base: 1,
        alt: 1,
        mask: 0x00,
    },
    Spec {
        base: 3,
        alt: 1,
        mask: 0x30,
    },
    Spec {
        base: 2,
        alt: 3,
        mask: 0x02,
    },
    Spec {
        base: 3,
        alt: 1,
        mask: 0x04,
    },
    Spec {
        base: 3,
        alt: 1,
        mask: 0x10,
    },
    Spec {
        base: 3,
        alt: 4,
        mask: 0x05,
    },
    Spec {
        base: 3,
        alt: 4,
        mask: 0x04,
    },
    Spec {
        base: 3,
        alt: 4,
        mask: 0x01,
    },
    Spec {
        base: 3,
        alt: 4,
        mask: 0x0c,
    },
    Spec {
        base: 2,
        alt: 3,
        mask: 0x20,
    },
    Spec {
        base: 3,
        alt: 4,
        mask: 0x44,
    },
    Spec {
        base: 3,
        alt: 2,
        mask: 0x44,
    },
    Spec {
        base: 3,
        alt: 4,
        mask: 0x40,
    },
    Spec {
        base: 3,
        alt: 4,
        mask: 0x14,
    },
    Spec {
        base: 3,
        alt: 4,
        mask: 0x10,
    },
];

fn render(pass: usize) -> [u8; WAVEFORM] {
    let mut wf = VENDOR;
    let mut specs = [HOLD; 4];
    for slot in 0..3 {
        let pos = pass * 3 + slot;
        if pos < TARGETS.len() {
            specs[slot + 1] = TARGETS[pos];
        }
    }
    for code in 0..4 {
        let spec = specs[code];
        let dest_begin = ANALOG + (code + 1) * TABLE;
        for record in 0..RECORDS {
            let table = if (spec.mask >> record) & 1 != 0 {
                spec.alt
            } else {
                spec.base
            };
            let src = ANALOG + table * TABLE + record * RECORD;
            let dst = dest_begin + record * RECORD;
            wf[dst..dst + RECORD].copy_from_slice(&VENDOR[src..src + RECORD]);
        }
    }
    wf
}

fn waveforms() -> &'static [[u8; WAVEFORM]; PASSES] {
    static W: OnceLock<[[u8; WAVEFORM]; PASSES]> = OnceLock::new();
    W.get_or_init(|| std::array::from_fn(render))
}

#[cfg(test)]
pub fn waveform(pass: usize) -> &'static [u8] {
    &waveforms()[pass]
}

/// `Some(pass)` only when `lut` is byte-identical to that render pass.
pub fn recognize(lut: &[u8]) -> Option<usize> {
    waveforms().iter().position(|wf| wf.as_slice() == lut)
}

/// Level painted by `code` (1..3) on `pass`. Code 0 holds, so it returns `None`.
pub fn level_of(pass: usize, code: u8) -> Option<u8> {
    if pass >= PASSES || !(1..=3).contains(&code) {
        return None;
    }
    let pos = pass * 3 + (code as usize - 1);
    (pos < 15).then_some(pos as u8)
}

/// 4-bit level to 8-bit luma. 0 → 0, 15 → 255.
#[cfg(test)]
pub fn gray4_luma(level: u8) -> u8 {
    level * 17
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv(bytes: &[u8]) -> u32 {
        bytes.iter().fold(0x811c_9dc5u32, |h, &b| {
            (h ^ b as u32).wrapping_mul(0x0100_0193)
        })
    }

    #[test]
    fn render_passes_match_the_demo_payloads() {
        assert_eq!(fnv(&VENDOR), 0x54f5_4e58);
        assert_eq!(
            [
                fnv(waveform(0)),
                fnv(waveform(1)),
                fnv(waveform(2)),
                fnv(waveform(3)),
                fnv(waveform(4))
            ],
            [
                0x7abd_ae2d,
                0x87f8_aec2,
                0x51b5_6fb5,
                0x1d59_36af,
                0x3078_f509
            ]
        );
        assert_eq!(
            &waveform(0)[..7],
            &[0x00, 0x14, 0x78, 0x78, 0x14, 0x85, 0x08]
        );
        // Code 0 is table 1; its drive bytes stay 0xFF so earlier levels are held.
        for record in 0..RECORDS {
            let off = ANALOG + TABLE + record * RECORD;
            assert_eq!((waveform(3)[off + 1], waveform(3)[off + 2]), (0xff, 0xff));
        }
        assert!(
            recognize(&VENDOR).is_none(),
            "the raw vendor table is not a render pass"
        );
        assert_eq!(recognize(waveform(4)), Some(4));
    }
}
