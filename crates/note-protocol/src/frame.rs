//! Canonical display frame (protocol v1, frozen for G3).
//!
//! The header is 64 bytes. Stride is not stored: pal2 packs 4 pixels per byte
//! and gray4 packs 2, high bits first, so the row size follows the width.
//! `epoch` stays a u32; it occupies bytes 8..12.

use thiserror::Error;

pub const HEADER_LEN: usize = 64;
pub const FORMAT_PAL2: u8 = 1;
pub const FORMAT_GRAY4: u8 = 2;
pub const SOURCE_PANEL: u8 = 1;
/// A legacy `HOME_EMULATOR` build's console frame (`BEGIN_FRAME` … `END_FRAME`).
pub const SOURCE_LEGACY_CONSOLE: u8 = 2;
pub const REFRESH_FULL: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHeader {
    pub instance_hash: u64,
    pub epoch: u32,
    pub seq: u64,
    /// `0` means the payload is a full image. Any other value is the sequence
    /// the receiver must already be holding.
    pub base_seq: u64,
    pub virtual_ns: u64,
    pub host_ns: u64,
    pub width: u16,
    pub height: u16,
    pub format: u8,
    pub palette_id: u8,
    pub source: u8,
    pub refresh: u8,
    pub dirty_x: u16,
    pub dirty_y: u16,
    pub dirty_w: u16,
    pub dirty_h: u16,
    pub pixel_hash: u32,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("frame header is {0} bytes, need {HEADER_LEN}")]
    Short(usize),
    #[error("frame format {0} is unknown")]
    Format(u8),
    #[error("pixel payload is {got} bytes, {width}x{height} format {format} needs {need}")]
    Length { got: usize, width: u16, height: u16, format: u8, need: usize },
    #[error("pixel hash {got:#x} does not match the payload {expected:#x}")]
    Hash { got: u32, expected: u32 },
    #[error("delta base {base} does not match the held sequence {held} (epoch {epoch})")]
    Unappliable { base: u64, held: u64, epoch: u32 },
    #[error("frame epoch {got} does not match the held epoch {held}")]
    Epoch { got: u32, held: u32 },
}

pub fn pixel_bytes(width: u16, height: u16, format: u8) -> Option<usize> {
    let pixels = width as usize * height as usize;
    match format {
        FORMAT_PAL2 if pixels % 4 == 0 => Some(pixels / 4),
        FORMAT_GRAY4 if pixels % 2 == 0 => Some(pixels / 2),
        _ => None,
    }
}

pub fn fnv1a(data: &[u8]) -> u32 {
    let mut hash = 0x811c9dc5u32;
    for byte in data {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

pub fn encode(header: &FrameHeader, pixels: &[u8]) -> Result<Vec<u8>, FrameError> {
    check_payload(header, pixels)?;
    if header.pixel_hash != fnv1a(pixels) {
        return Err(FrameError::Hash { got: header.pixel_hash, expected: fnv1a(pixels) });
    }
    let mut out = Vec::with_capacity(HEADER_LEN + pixels.len());
    out.extend_from_slice(&header.instance_hash.to_le_bytes());
    out.extend_from_slice(&header.epoch.to_le_bytes());
    out.extend_from_slice(&header.seq.to_le_bytes());
    out.extend_from_slice(&header.base_seq.to_le_bytes());
    out.extend_from_slice(&header.virtual_ns.to_le_bytes());
    out.extend_from_slice(&header.host_ns.to_le_bytes());
    out.extend_from_slice(&header.width.to_le_bytes());
    out.extend_from_slice(&header.height.to_le_bytes());
    out.push(header.format);
    out.push(header.palette_id);
    out.push(header.source);
    out.push(header.refresh);
    for v in [header.dirty_x, header.dirty_y, header.dirty_w, header.dirty_h] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&header.pixel_hash.to_le_bytes());
    debug_assert_eq!(out.len(), HEADER_LEN);
    out.extend_from_slice(pixels);
    Ok(out)
}

pub fn decode(buf: &[u8]) -> Result<(FrameHeader, Vec<u8>), FrameError> {
    if buf.len() < HEADER_LEN {
        return Err(FrameError::Short(buf.len()));
    }
    let header = FrameHeader {
        instance_hash: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
        epoch: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        seq: u64::from_le_bytes(buf[12..20].try_into().unwrap()),
        base_seq: u64::from_le_bytes(buf[20..28].try_into().unwrap()),
        virtual_ns: u64::from_le_bytes(buf[28..36].try_into().unwrap()),
        host_ns: u64::from_le_bytes(buf[36..44].try_into().unwrap()),
        width: u16::from_le_bytes(buf[44..46].try_into().unwrap()),
        height: u16::from_le_bytes(buf[46..48].try_into().unwrap()),
        format: buf[48],
        palette_id: buf[49],
        source: buf[50],
        refresh: buf[51],
        dirty_x: u16::from_le_bytes(buf[52..54].try_into().unwrap()),
        dirty_y: u16::from_le_bytes(buf[54..56].try_into().unwrap()),
        dirty_w: u16::from_le_bytes(buf[56..58].try_into().unwrap()),
        dirty_h: u16::from_le_bytes(buf[58..60].try_into().unwrap()),
        pixel_hash: u32::from_le_bytes(buf[60..64].try_into().unwrap()),
    };
    let pixels = buf[HEADER_LEN..].to_vec();
    check_payload(&header, &pixels)?;
    let expected = fnv1a(&pixels);
    if header.pixel_hash != expected {
        return Err(FrameError::Hash { got: header.pixel_hash, expected });
    }
    Ok((header, pixels))
}

/// Apply a frame to the image the client already holds.
/// A full frame (`base_seq == 0`) replaces it. A delta applies only when
/// `base_seq` is the sequence currently held and the epoch matches.
pub fn apply(held_epoch: u32, held_seq: u64, _held: &[u8], header: &FrameHeader, pixels: &[u8]) -> Result<Vec<u8>, FrameError> {
    check_payload(header, pixels)?;
    // A new epoch (restart, snapshot restore) is entered with a full frame; only a delta has
    // to belong to the held epoch.
    if header.base_seq != 0 && header.epoch != held_epoch && held_seq != 0 {
        return Err(FrameError::Epoch { got: header.epoch, held: held_epoch });
    }
    if header.base_seq != 0 && header.base_seq != held_seq {
        return Err(FrameError::Unappliable { base: header.base_seq, held: held_seq, epoch: held_epoch });
    }
    Ok(pixels.to_vec())
}

fn check_payload(header: &FrameHeader, pixels: &[u8]) -> Result<(), FrameError> {
    let Some(need) = pixel_bytes(header.width, header.height, header.format) else {
        return Err(FrameError::Format(header.format));
    };
    if pixels.len() != need {
        return Err(FrameError::Length { got: pixels.len(), width: header.width, height: header.height, format: header.format, need });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(base: u64, pixels: &[u8]) -> FrameHeader {
        FrameHeader {
            instance_hash: 1, epoch: 1, seq: 2, base_seq: base, virtual_ns: 3, host_ns: 4,
            width: 4, height: 2, format: FORMAT_GRAY4, palette_id: 0, source: SOURCE_PANEL,
            refresh: REFRESH_FULL, dirty_x: 0, dirty_y: 0, dirty_w: 4, dirty_h: 2,
            pixel_hash: fnv1a(pixels),
        }
    }

    #[test]
    fn roundtrip_is_64_bytes_plus_pixels_and_a_bad_delta_is_rejected() {
        let pixels = [1u8, 2, 3, 4];
        let bytes = encode(&header(0, &pixels), &pixels).unwrap();
        assert_eq!(bytes.len(), HEADER_LEN + 4);
        let (decoded, got) = decode(&bytes).unwrap();
        assert_eq!(got, pixels);
        assert_eq!(decoded.seq, 2);
        let applied = apply(1, 0, &[], &decoded, &got).unwrap();
        assert_eq!(applied, pixels);

        let next = [9u8, 9, 9, 9];
        let delta = header(1, &next); // claims the client holds seq 1
        let err = apply(1, 7, &pixels, &delta, &next).unwrap_err();
        assert!(matches!(err, FrameError::Unappliable { held: 7, .. }));
        let replaced = apply(1, 1, &pixels, &delta, &next).unwrap();
        assert_eq!(replaced, next);
    }

    #[test]
    fn a_new_epoch_is_entered_with_a_full_frame_and_never_with_a_delta() {
        let pixels = [5u8, 6, 7, 8];
        let mut full = header(0, &pixels);
        full.epoch = 2;
        assert_eq!(apply(1, 9, &[0; 4], &full, &pixels).unwrap(), pixels, "restore: full frame, new epoch");
        let mut delta = header(9, &pixels);
        delta.epoch = 2;
        assert!(matches!(apply(1, 9, &[0; 4], &delta, &pixels).unwrap_err(), FrameError::Epoch { got: 2, held: 1 }));
    }
}
