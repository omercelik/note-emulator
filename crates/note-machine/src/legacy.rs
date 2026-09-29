//! Legacy `HOME_EMULATOR` builds draw over the serial console instead of the panel (D8,
//! previews only): `BEGIN_FRAME <w> <h>`, one base64 line per row of 2 bpp pixels (high bits
//! first, the pal2 packing), `END_FRAME`. The filter takes those blocks out of the console
//! stream, like the old runner did, and keeps the last complete frame.

const BEGIN: &[u8] = b"BEGIN_FRAME";

#[derive(Default)]
pub struct LegacyConsole {
    /// Bytes of the current line held back because they may start `BEGIN_FRAME`.
    pending: Vec<u8>,
    line_start: bool,
    in_frame: Option<(usize, usize)>,
    row: Vec<u8>,
    rows: Vec<Vec<u8>>,
    /// Last complete frame (w, h, pal2 bytes) and how many decoded so far.
    pub frame: Option<(usize, usize, Vec<u8>)>,
    pub frames: u64,
    pub rejected: u64,
}

impl LegacyConsole {
    pub fn new() -> LegacyConsole {
        LegacyConsole { line_start: true, ..Default::default() }
    }

    /// Console bytes with frame blocks removed. A frame that completes here updates `frame`.
    pub fn filter(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for &b in bytes {
            if self.in_frame.is_some() {
                if b == b'\n' {
                    let line = trim_cr(std::mem::take(&mut self.row));
                    self.frame_line(line);
                } else {
                    self.row.push(b);
                }
                continue;
            }
            if self.line_start || !self.pending.is_empty() {
                self.pending.push(b);
                self.line_start = false;
                if b == b'\n' {
                    let line = trim_cr(self.pending[..self.pending.len() - 1].to_vec());
                    if let Some(size) = parse_begin(&line) {
                        self.in_frame = Some(size);
                        self.rows.clear();
                    } else {
                        out.extend_from_slice(&self.pending);
                    }
                    self.pending.clear();
                    self.line_start = true;
                } else if !(BEGIN.starts_with(&self.pending) || self.pending.starts_with(BEGIN)) {
                    out.append(&mut self.pending);
                }
                continue;
            }
            out.push(b);
            self.line_start = b == b'\n';
        }
        out
    }

    fn frame_line(&mut self, line: Vec<u8>) {
        if line == b"END_FRAME" {
            let (w, h) = self.in_frame.take().unwrap();
            let decoded: Option<Vec<u8>> = self.rows.iter().try_fold(Vec::new(), |mut acc, row| {
                acc.extend(base64(row)?);
                Some(acc)
            });
            match decoded {
                Some(bytes) if bytes.len() == w * h / 4 => {
                    self.frame = Some((w, h, bytes));
                    self.frames += 1;
                }
                _ => self.rejected += 1,
            }
            self.rows.clear();
        } else if let Some(size) = parse_begin(&line) {
            // A new BEGIN while collecting: the previous frame was abandoned mid-write.
            self.in_frame = Some(size);
            self.rows.clear();
        } else {
            self.rows.push(line);
        }
    }
}

fn trim_cr(mut line: Vec<u8>) -> Vec<u8> {
    while line.last() == Some(&b'\r') {
        line.pop();
    }
    line
}

fn parse_begin(line: &[u8]) -> Option<(usize, usize)> {
    let text = std::str::from_utf8(line).ok()?;
    let mut parts = text.split_whitespace();
    if parts.next()? != "BEGIN_FRAME" {
        return None;
    }
    let w: usize = parts.next()?.parse().ok()?;
    let h: usize = parts.next()?.parse().ok()?;
    (parts.next().is_none() && w > 0 && h > 0 && w * h <= 1 << 22 && (w * h) % 4 == 0).then_some((w, h))
}

/// Standard base64 with padding; each row is padded on its own.
fn base64(line: &[u8]) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    if line.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(line.len() / 4 * 3);
    for chunk in line.chunks(4) {
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 {
            return None;
        }
        let mut n = 0u32;
        for &c in &chunk[..4 - pad] {
            n = (n << 6) | val(c)?;
        }
        n <<= 6 * pad as u32;
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(bytes: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in bytes.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                s.push(if i <= c.len() { A[(n >> (18 - 6 * i)) as usize & 63] as char } else { '=' });
            }
        }
        s
    }

    fn frame_text(w: usize, h: usize, fill: impl Fn(usize) -> u8) -> String {
        let mut t = format!("BEGIN_FRAME {w} {h}\r\n");
        for y in 0..h {
            let row: Vec<u8> = (0..w / 4).map(|x| fill(y * w / 4 + x)).collect();
            t.push_str(&encode(&row));
            t.push_str("\r\n");
        }
        t.push_str("END_FRAME\r\n");
        t
    }

    #[test]
    fn frames_leave_the_console_and_decode_row_by_row() {
        let mut c = LegacyConsole::new();
        let text = format!("I (1) app: hello\nBEGIN\n{}prompt> ", frame_text(400, 300, |i| (i % 251) as u8));
        // Split across calls, including inside the BEGIN line and the rows.
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        for chunk in bytes.chunks(7) {
            out.extend(c.filter(chunk));
        }
        assert_eq!(String::from_utf8(out).unwrap(), "I (1) app: hello\nBEGIN\nprompt> ");
        let (w, h, px) = c.frame.clone().unwrap();
        assert_eq!((w, h, px.len(), c.frames), (400, 300, 30000, 1));
        assert!(px.iter().enumerate().all(|(i, &b)| b == (i % 251) as u8));
    }

    #[test]
    fn an_abandoned_or_short_frame_is_not_shown() {
        let mut c = LegacyConsole::new();
        let good = frame_text(8, 2, |_| 0x55);
        let abandoned = "BEGIN_FRAME 8 2\nAAAA\n";
        c.filter(format!("{abandoned}{good}").as_bytes());
        assert_eq!((c.frames, c.rejected), (1, 0), "a second BEGIN restarts the frame");
        c.filter(b"BEGIN_FRAME 8 2\nVVU=\nEND_FRAME\n");
        assert_eq!((c.frames, c.rejected), (1, 1), "one row of two is too short");
        assert_eq!(c.frame.as_ref().unwrap().2, vec![0x55; 4]);
    }
}
