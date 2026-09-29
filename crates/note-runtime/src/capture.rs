//! PNG and GIF of the canonical panel frames `note-emu` already exports.
//! A committed state is held until the next commit. Pause does not add a frame;
//! the hold simply continues. MP4 is not produced: this workspace has no encoder.

use std::collections::HashMap;
use std::io::{self, Cursor};

use note_core::profile::Display;
use note_core::DisplayFormat;

/// One committed panel image and the virtual time it became current.
#[derive(Clone, Debug)]
struct Held {
    indices: Vec<u8>,
    start_ns: u64,
    end_ns: Option<u64>,
}

/// Records committed frames for PNG (one state) and GIF (the timeline).
#[derive(Clone, Debug)]
pub struct FrameCapture {
    width: u16,
    height: u16,
    format: DisplayFormat,
    palette: Vec<[u8; 3]>,
    frames: Vec<Held>,
    paused: bool,
}

impl FrameCapture {
    pub fn new(width: u32, height: u32, format: DisplayFormat, palette: Vec<[u8; 3]>) -> Self {
        FrameCapture {
            width: width.min(u16::MAX as u32) as u16,
            height: height.min(u16::MAX as u32) as u16,
            format,
            palette,
            frames: Vec::new(),
            paused: false,
        }
    }

    pub fn from_display(display: &Display) -> Self {
        Self::new(display.width, display.height, display.format, palette_of(display))
    }

    pub fn frame_count(&self) -> usize { self.frames.len() }
    pub fn is_paused(&self) -> bool { self.paused }

    /// Remember a new committed image. Ignored while paused, so a pause cannot invent one.
    pub fn commit(&mut self, visible: &[u8], t_ns: u64) {
        if self.paused { return; }
        if let Some(open) = self.frames.last_mut() {
            if open.end_ns.is_none() { open.end_ns = Some(t_ns); }
        }
        self.frames.push(Held { indices: indices(self.format, self.width as usize, self.height as usize, visible), start_ns: t_ns, end_ns: None });
    }

    pub fn pause(&mut self, _t_ns: u64) { self.paused = true; }
    pub fn resume(&mut self, _t_ns: u64) { self.paused = false; }

    /// Close the last hold at `t_ns` without adding a frame.
    pub fn finish(&mut self, t_ns: u64) {
        if let Some(open) = self.frames.last_mut() {
            if open.end_ns.is_none() { open.end_ns = Some(t_ns.max(open.start_ns)); }
        }
    }

    pub fn png(&self, which: usize) -> io::Result<Vec<u8>> {
        let frame = self.frames.get(which).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such frame"))?;
        encode_png_rgb(self.width as u32, self.height as u32, &self.rgb(&frame.indices))
    }

    /// GIF89a. Each committed state is one image; its delay is how long it stayed current,
    /// including time spent paused. Delay is in centiseconds, the GIF unit.
    pub fn gif(&self) -> io::Result<Vec<u8>> {
        if self.frames.is_empty() { return Err(io::Error::new(io::ErrorKind::InvalidInput, "no frames")); }
        let ncolors = self.palette.len().next_power_of_two().clamp(2, 256);
        let min_code = ncolors.trailing_zeros().max(2) as u8;
        let mut table = self.palette.clone();
        table.resize(ncolors, [0, 0, 0]);
        let size_field = (ncolors.trailing_zeros() - 1) as u8;
        let mut out = Vec::new();
        out.extend_from_slice(b"GIF89a");
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.push(0x80 | size_field); // global color table
        out.push(0);
        out.push(0);
        for rgb in &table { out.extend_from_slice(rgb); }
        for frame in &self.frames {
            let end = frame.end_ns.unwrap_or(frame.start_ns);
            let mut delay_cs = ((end.saturating_sub(frame.start_ns) + 5_000_000) / 10_000_000).min(u16::MAX as u64) as u16;
            if end > frame.start_ns && delay_cs == 0 { delay_cs = 1; }
            // Graphic control: disposal = do not dispose, no transparent index.
            out.extend_from_slice(&[0x21, 0xF9, 0x04, 0x04]);
            out.extend_from_slice(&delay_cs.to_le_bytes());
            out.extend_from_slice(&[0x00, 0x00]);
            out.push(0x2C);
            out.extend_from_slice(&[0, 0, 0, 0]);
            out.extend_from_slice(&self.width.to_le_bytes());
            out.extend_from_slice(&self.height.to_le_bytes());
            out.push(0x00);
            let lzw = lzw_encode(&frame.indices, min_code);
            out.push(min_code);
            for chunk in lzw.chunks(255) {
                out.push(chunk.len() as u8);
                out.extend_from_slice(chunk);
            }
            out.push(0);
        }
        out.push(0x3B);
        Ok(out)
    }

    fn rgb(&self, indices: &[u8]) -> Vec<u8> {
        let mut rgb = Vec::with_capacity(indices.len() * 3);
        for &ix in indices {
            let color = self.palette.get(ix as usize).copied().unwrap_or([0, 0, 0]);
            rgb.extend_from_slice(&color);
        }
        rgb
    }
}

pub fn palette_of(display: &Display) -> Vec<[u8; 3]> {
    match display.format {
        DisplayFormat::Pal2 => display.palette.iter().map(|hex| parse_hex(hex)).collect(),
        DisplayFormat::Gray4 => (0..16u8).map(|level| [level * 17; 3]).collect(),
    }
}

pub fn parse_hex(hex: &str) -> [u8; 3] {
    let h = hex.trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    [byte(0), byte(2), byte(4)]
}

/// Same pixel walk as `note-emu`'s PNG export.
pub fn indices(format: DisplayFormat, width: usize, height: usize, visible: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(width.saturating_mul(height));
    for i in 0..width.saturating_mul(height) {
        let index = match format {
            DisplayFormat::Pal2 => visible.get(i / 4).map(|b| (b >> (6 - 2 * (i % 4))) & 3).unwrap_or(0),
            DisplayFormat::Gray4 => visible.get(i / 2).map(|b| if i % 2 == 0 { b >> 4 } else { b & 0x0f }).unwrap_or(0),
        };
        out.push(index);
    }
    out
}

pub fn encode_png(display: &Display, visible: &[u8]) -> io::Result<Vec<u8>> {
    let palette = palette_of(display);
    let indexed = indices(display.format, display.width as usize, display.height as usize, visible);
    let mut rgb = Vec::with_capacity(indexed.len() * 3);
    for ix in indexed {
        rgb.extend_from_slice(&palette.get(ix as usize).copied().unwrap_or([0, 0, 0]));
    }
    encode_png_rgb(display.width, display.height, &rgb)
}

fn encode_png_rgb(width: u32, height: u32, rgb: &[u8]) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut enc = png::Encoder::new(Cursor::new(&mut buf), width, height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(io::Error::other)?;
    writer.write_image_data(rgb).map_err(io::Error::other)?;
    drop(writer);
    Ok(buf)
}

fn lzw_encode(indices: &[u8], min_code_size: u8) -> Vec<u8> {
    let clear = 1u16 << min_code_size;
    let eoi = clear + 1;
    let mut code_size = min_code_size as u32 + 1;
    let mut next: u16 = eoi + 1;
    let mut dict: HashMap<(u16, u8), u16> = HashMap::new();
    let mut bits: u64 = 0;
    let mut nbits: u32 = 0;
    let mut out = Vec::new();
    let emit = |code: u16, code_size: u32, bits: &mut u64, nbits: &mut u32, out: &mut Vec<u8>| {
        *bits |= (code as u64) << *nbits;
        *nbits += code_size;
        while *nbits >= 8 {
            out.push(*bits as u8);
            *bits >>= 8;
            *nbits -= 8;
        }
    };
    let reset = |code_size: &mut u32, next: &mut u16, dict: &mut HashMap<(u16, u8), u16>| {
        dict.clear();
        *code_size = min_code_size as u32 + 1;
        *next = eoi + 1;
    };
    emit(clear, code_size, &mut bits, &mut nbits, &mut out);
    if indices.is_empty() {
        emit(eoi, code_size, &mut bits, &mut nbits, &mut out);
        if nbits > 0 { out.push(bits as u8); }
        return out;
    }
    let mut prefix = indices[0] as u16;
    for &k in &indices[1..] {
        if let Some(&code) = dict.get(&(prefix, k)) {
            prefix = code;
            continue;
        }
        emit(prefix, code_size, &mut bits, &mut nbits, &mut out);
        if next < 4096 {
            dict.insert((prefix, k), next);
            next += 1;
            if next == 4096 {
                emit(clear, code_size, &mut bits, &mut nbits, &mut out);
                reset(&mut code_size, &mut next, &mut dict);
            } else if code_size < 12 && next == (1u16 << code_size) {
                code_size += 1;
            }
        }
        prefix = k as u16;
    }
    emit(prefix, code_size, &mut bits, &mut nbits, &mut out);
    emit(eoi, code_size, &mut bits, &mut nbits, &mut out);
    if nbits > 0 { out.push(bits as u8); }
    out
}

/// Test helper and a check that the encoder's delays and pixels survive a reader
/// that does not share the encoder's dictionary code.
#[cfg(test)]
pub(crate) fn decode_gif(bytes: &[u8]) -> Result<Vec<(u16, Vec<u8>)>, String> {
    if bytes.len() < 13 || &bytes[0..6] != b"GIF89a" { return Err("not a GIF89a".into()); }
    let width = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
    let height = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
    let packed = bytes[10];
    if packed & 0x80 == 0 { return Err("GIF has no global color table".into()); }
    let ncolors = 1usize << ((packed & 7) + 1);
    let mut i = 13 + ncolors * 3;
    let mut frames = Vec::new();
    while i < bytes.len() && bytes[i] != 0x3B {
        if bytes[i] != 0x21 || bytes.get(i + 1) != Some(&0xF9) { return Err(format!("expected graphic control at {i}")); }
        let delay = u16::from_le_bytes([bytes[i + 4], bytes[i + 5]]);
        i += 8;
        if bytes.get(i) != Some(&0x2C) { return Err("expected image descriptor".into()); }
        i += 10;
        let min_code = *bytes.get(i).ok_or("truncated image data")? as u32;
        i += 1;
        let mut lzw = Vec::new();
        loop {
            let n = *bytes.get(i).ok_or("truncated sub-block")? as usize;
            i += 1;
            if n == 0 { break; }
            let end = i + n;
            if end > bytes.len() { return Err("sub-block overruns".into()); }
            lzw.extend_from_slice(&bytes[i..end]);
            i = end;
        }
        let indices = lzw_decode(&lzw, min_code, width * height)?;
        frames.push((delay, indices));
    }
    Ok(frames)
}

#[cfg(test)]
fn lzw_decode(data: &[u8], min_code_size: u32, expected: usize) -> Result<Vec<u8>, String> {
    let clear = 1u32 << min_code_size;
    let eoi = clear + 1;
    let mut code_size = min_code_size + 1;
    let mut next = eoi + 1;
    // Index equals LZW code; clear and EOI have no decoded bytes.
    let mut table: Vec<Vec<u8>> = (0..next as usize).map(|code| if (code as u32) < clear { vec![code as u8] } else { Vec::new() }).collect();
    let mut bits: u64 = 0;
    let mut nbits: u32 = 0;
    let mut at = 0usize;
    let pull = |code_size: u32, bits: &mut u64, nbits: &mut u32, at: &mut usize| -> Option<u32> {
        while *nbits < code_size {
            if *at >= data.len() { return None; }
            *bits |= (data[*at] as u64) << *nbits;
            *at += 1;
            *nbits += 8;
        }
        let code = (*bits & ((1u64 << code_size) - 1)) as u32;
        *bits >>= code_size;
        *nbits -= code_size;
        Some(code)
    };
    let mut out = Vec::with_capacity(expected);
    let first = pull(code_size, &mut bits, &mut nbits, &mut at).ok_or("missing clear")?;
    if first != clear { return Err("stream does not start with clear".into()); }
    let mut prev: Option<u32> = None;
    loop {
        let Some(code) = pull(code_size, &mut bits, &mut nbits, &mut at) else { break };
        if code == clear {
            code_size = min_code_size + 1;
            next = eoi + 1;
            table.truncate(next as usize);
            prev = None;
            continue;
        }
        if code == eoi { break; }
        let entry = if (code as usize) < table.len() && !table[code as usize].is_empty() {
            table[code as usize].clone()
        } else if code == next {
            let mut e = table[prev.ok_or("kwkw without prev")? as usize].clone();
            let first_b = *e.first().ok_or("empty prev")?;
            e.push(first_b);
            e
        } else {
            return Err(format!("bad code {code}"));
        };
        out.extend_from_slice(&entry);
        if let Some(p) = prev {
            let mut added = table[p as usize].clone();
            added.push(entry[0]);
            table.push(added);
            next += 1;
            if next == 4096 {
                // The encoder emits clear next; size stays 12 until that code is read.
            } else if code_size < 12 && next == (1 << code_size) - 1 {
                code_size += 1;
            }
        }
        prev = Some(code);
    }
    if out.len() != expected { return Err(format!("decoded {} pixels, want {expected}", out.len())); }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pal2() -> Vec<[u8; 3]> {
        vec![[0, 0, 0], [255, 255, 255], [255, 255, 0], [255, 0, 0]]
    }

    fn pixels(bytes: &[u8]) -> Vec<u8> {
        indices(DisplayFormat::Pal2, 4, 1, bytes)
    }

    #[test]
    fn png_matches_the_selected_frame_and_gif_holds_two_states_across_pause() {
        // Four pal2 pixels: black, white, yellow, red.
        let state_a = [0b00_01_10_11];
        let state_b = [0b11_10_01_00];
        let mut cap = FrameCapture::new(4, 1, DisplayFormat::Pal2, pal2());
        cap.commit(&state_a, 0);
        cap.pause(100_000_000);
        cap.commit(&state_b, 200_000_000); // ignored: pause must not invent a frame
        assert_eq!(cap.frame_count(), 1);
        cap.resume(400_000_000);
        cap.commit(&state_b, 500_000_000);
        cap.finish(700_000_000);
        assert_eq!(cap.frame_count(), 2);

        let png = cap.png(0).unwrap();
        let decoded = decode_png(&png);
        let expect: Vec<u8> = pixels(&state_a).into_iter().flat_map(|ix| pal2()[ix as usize]).collect();
        assert_eq!(decoded, expect, "PNG pixels are the selected frame");
        let png_b = cap.png(1).unwrap();
        let expect_b: Vec<u8> = pixels(&state_b).into_iter().flat_map(|ix| pal2()[ix as usize]).collect();
        assert_eq!(decode_png(&png_b), expect_b);

        let frames = decode_gif(&cap.gif().unwrap()).unwrap();
        assert_eq!(frames.len(), 2, "pause did not insert a frame");
        assert_eq!(frames[0].0, 50, "state A held 500 ms, including the pause");
        assert_eq!(frames[1].0, 20, "state B held 200 ms");
        assert_eq!(frames[0].1, pixels(&state_a));
        assert_eq!(frames[1].1, pixels(&state_b));
    }

    #[test]
    fn gray4_png_uses_level_times_seventeen() {
        let display = note_core::profile::Display {
            panel: note_core::PanelKind::Ssd2683,
            variant: note_core::PanelVariant::Mono,
            width: 2,
            height: 1,
            format: DisplayFormat::Gray4,
            palette: Vec::new(),
            palette_names: Vec::new(),
            gray_levels: Some(16),
            spi_host: 1,
            pins: note_core::profile::PanelPins { power: 0, busy: 0, reset: 0, dc: 0, cs: 0, sclk: 0, mosi: 0 },
            readback_on_mosi: false,
        };
        // High nibble is the left pixel. 0 and 15.
        let png = encode_png(&display, &[0x0F]).unwrap();
        assert_eq!(decode_png(&png), vec![0, 0, 0, 255, 255, 255]);
    }

    fn decode_png(bytes: &[u8]) -> Vec<u8> {
        let dec = png::Decoder::new(Cursor::new(bytes));
        let mut reader = dec.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        buf
    }
}
