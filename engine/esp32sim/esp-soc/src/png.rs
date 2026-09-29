#![allow(non_snake_case)]
//! A minimal PNG writer for display captures.
/// Minimal PNG writer (RGB565 -> RGB8, uncompressed deflate blocks) — no zlib dependency.
pub fn write_png_rgb565(path: &str, px: &[u16], w: usize, h: usize, scale: usize) -> std::io::Result<()> {
    fn crc32(data: &[u8]) -> u32 { let mut c = 0xffff_ffffu32; for &b in data { c ^= b as u32; for _ in 0..8 { c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 }; } } !c }
    fn adler(data: &[u8]) -> u32 { let (mut a, mut b) = (1u32, 0u32); for &d in data { a = (a + d as u32) % 65521; b = (b + a) % 65521; } (b << 16) | a }
    fn chunk(out: &mut Vec<u8>, t: &[u8], d: &[u8]) { out.extend_from_slice(&(d.len() as u32).to_be_bytes()); let mut td = t.to_vec(); td.extend_from_slice(d); out.extend_from_slice(&td); out.extend_from_slice(&crc32(&td).to_be_bytes()); }
    let (W, H) = (w * scale, h * scale);
    let mut raw = Vec::with_capacity(H * (W * 3 + 1));
    for y in 0..H { raw.push(0); for x in 0..W { let p = px[(y / scale) * w + x / scale] as u32; raw.push(((p >> 11) * 255 / 31) as u8); raw.push((((p >> 5) & 63) * 255 / 63) as u8); raw.push(((p & 31) * 255 / 31) as u8); } }
    // zlib stream with stored (uncompressed) deflate blocks
    let mut z = vec![0x78, 0x01];
    for (i, blk) in raw.chunks(65535).enumerate() { let last = (i + 1) * 65535 >= raw.len(); z.push(last as u8); z.extend_from_slice(&(blk.len() as u16).to_le_bytes()); z.extend_from_slice(&(!(blk.len() as u16)).to_le_bytes()); z.extend_from_slice(blk); }
    z.extend_from_slice(&adler(&raw).to_be_bytes());
    let mut out = vec![137, 80, 78, 71, 13, 10, 26, 10];
    let mut ihdr = Vec::new(); ihdr.extend_from_slice(&(W as u32).to_be_bytes()); ihdr.extend_from_slice(&(H as u32).to_be_bytes()); ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr); chunk(&mut out, b"IDAT", &z); chunk(&mut out, b"IEND", &[]);
    std::fs::write(path, out)
}
