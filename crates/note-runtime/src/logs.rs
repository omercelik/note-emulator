//! Raw serial capture and the merged view (Spec §11.2).
//!
//! Raw bytes are kept per channel before parsing. The merged view collapses a
//! line only when the other channel repeats it inside the twin window. The same
//! channel keeps every copy. Clearing the view does not delete the capture.
//! Rotation drops the oldest bytes and records a gap; an export then says the
//! history is not lossless.

use note_machine::replay::TWIN_WINDOW_NS;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gap {
    pub channel: u8,
    pub dropped_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub seq: u64,
    pub channel: u8,
    pub virtual_ns: u64,
    pub raw: Vec<u8>,
    pub level: Option<char>,
    pub tag: Option<String>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergedLine {
    pub seqs: Vec<u64>,
    pub channels: Vec<u8>,
    pub virtual_ns: u64,
    pub level: Option<char>,
    pub tag: Option<String>,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub channel: Option<u8>,
    pub level: Option<char>,
    pub tag: Option<String>,
    pub search: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Export {
    pub lossless: bool,
    pub gaps: Vec<Gap>,
    pub lines: Vec<MergedLine>,
    pub from_seq: u64,
    pub to_seq: u64,
}

pub struct Capture {
    events: Vec<Event>,
    partial: [Vec<u8>; 2],
    next_seq: u64,
    view_floor: u64,
    limit: usize,
    gaps: Vec<Gap>,
}

impl Capture {
    pub fn new(limit: usize) -> Capture {
        Capture { events: Vec::new(), partial: [Vec::new(), Vec::new()], next_seq: 1, view_floor: 0, limit, gaps: Vec::new() }
    }

    pub fn ingest(&mut self, channel: u8, virtual_ns: u64, bytes: &[u8]) {
        let ch = channel as usize;
        if ch >= self.partial.len() {
            return;
        }
        self.partial[ch].extend_from_slice(bytes);
        while let Some(end) = self.partial[ch].iter().position(|b| *b == b'\n') {
            let mut line = self.partial[ch].drain(..=end).collect::<Vec<_>>();
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.push_line(channel, virtual_ns, line);
        }
        self.trim(channel);
    }

    pub fn clear_view(&mut self) {
        self.view_floor = self.next_seq.saturating_sub(1);
    }

    pub fn raw(&self, channel: u8) -> Vec<u8> {
        let mut out = Vec::new();
        for ev in self.events.iter().filter(|e| e.channel == channel) {
            out.extend_from_slice(&ev.raw);
            out.push(b'\n');
        }
        out.extend_from_slice(&self.partial[channel as usize]);
        out
    }

    pub fn export(&self, filter: &Filter, view: bool) -> Export {
        let lines = self.query(filter, view);
        let from_seq = self.events.first().map(|e| e.seq).unwrap_or(0);
        let to_seq = self.events.last().map(|e| e.seq).unwrap_or(0);
        Export { lossless: self.gaps.is_empty(), gaps: self.gaps.clone(), lines, from_seq, to_seq }
    }

    pub fn query(&self, filter: &Filter, view: bool) -> Vec<MergedLine> {
        self.merged(view).into_iter().filter(|line| filter.matches(line)).collect()
    }

    pub fn merged(&self, view: bool) -> Vec<MergedLine> {
        let events: Vec<&Event> = self.events.iter().filter(|e| !view || e.seq > self.view_floor).collect();
        let mut used = vec![false; events.len()];
        let mut out = Vec::new();
        for i in 0..events.len() {
            if used[i] {
                continue;
            }
            let ev = events[i];
            let twin = events.iter().enumerate().skip(i + 1).find(|(j, other)| {
                !used[*j]
                    && other.channel != ev.channel
                    && other.raw == ev.raw
                    && other.virtual_ns.abs_diff(ev.virtual_ns) <= TWIN_WINDOW_NS
            });
            if let Some((j, other)) = twin {
                used[j] = true;
                out.push(MergedLine {
                    seqs: vec![ev.seq, other.seq],
                    channels: vec![ev.channel, other.channel],
                    virtual_ns: ev.virtual_ns,
                    level: ev.level,
                    tag: ev.tag.clone(),
                    text: ev.text.clone(),
                });
            } else {
                out.push(MergedLine {
                    seqs: vec![ev.seq],
                    channels: vec![ev.channel],
                    virtual_ns: ev.virtual_ns,
                    level: ev.level,
                    tag: ev.tag.clone(),
                    text: ev.text.clone(),
                });
            }
        }
        out
    }

    pub fn events_after(&self, seq: u64) -> impl Iterator<Item = &Event> {
        self.events.iter().filter(move |e| e.seq > seq)
    }

    fn push_line(&mut self, channel: u8, virtual_ns: u64, raw: Vec<u8>) {
        let lossy = String::from_utf8_lossy(&raw);
        let stripped = strip_ansi(&lossy);
        let parsed = parse_idf(&stripped);
        let (level, tag, text) = match parsed {
            Some((level, tag, message)) => (Some(level), Some(tag), message),
            None => (None, None, stripped),
        };
        self.events.push(Event { seq: self.next_seq, channel, virtual_ns, raw, level, tag, text });
        self.next_seq += 1;
    }

    fn trim(&mut self, channel: u8) {
        let mut dropped = 0usize;
        while self.retained(channel) > self.limit {
            let Some(i) = self.events.iter().position(|e| e.channel == channel) else { break };
            dropped += self.events[i].raw.len() + 1;
            self.events.remove(i);
        }
        if dropped > 0 {
            self.gaps.push(Gap { channel, dropped_bytes: dropped });
        }
    }

    fn retained(&self, channel: u8) -> usize {
        self.events.iter().filter(|e| e.channel == channel).map(|e| e.raw.len() + 1).sum()
    }
}

impl Filter {
    fn matches(&self, line: &MergedLine) -> bool {
        if let Some(ch) = self.channel {
            if !line.channels.contains(&ch) {
                return false;
            }
        }
        if let Some(level) = self.level {
            if line.level != Some(level) {
                return false;
            }
        }
        if let Some(tag) = &self.tag {
            if line.tag.as_deref() != Some(tag.as_str()) {
                return false;
            }
        }
        if let Some(search) = &self.search {
            if !line.text.contains(search) {
                return false;
            }
        }
        true
    }
}

fn strip_ansi(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'm' {
                i += 1;
            }
            i = (i + 1).min(bytes.len());
            continue;
        }
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn parse_idf(line: &str) -> Option<(char, String, String)> {
    let mut chars = line.chars();
    let level = chars.next()?;
    if !matches!(level, 'E' | 'W' | 'I' | 'D' | 'V') {
        return None;
    }
    let rest = chars.as_str().trim_start();
    let rest = rest.strip_prefix('(')?;
    let (ticks, after) = rest.split_once(')')?;
    if !ticks.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let after = after.trim_start();
    let (tag, message) = after.split_once(':')?;
    Some((level, tag.trim().to_string(), message.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twins_collapse_and_same_channel_repeats_stay_in_the_raw_capture() {
        let mut cap = Capture::new(4096);
        cap.ingest(0, 0, b"boot\n");
        cap.ingest(1, 1_000, b"boot\n");
        cap.ingest(0, 2_000, b"boot\n");
        cap.ingest(0, 3_000, b"\x1b[0mI (1) app: ready\n");
        let merged = cap.merged(false);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].channels, vec![0, 1]);
        assert_eq!(merged[0].seqs.len(), 2);
        assert_eq!(merged[1].channels, vec![0]);
        assert_eq!(merged[1].text, "boot");
        assert_eq!(merged[2].level, Some('I'));
        assert_eq!(merged[2].tag.as_deref(), Some("app"));
        assert_eq!(merged[2].text, "ready");
        assert!(cap.raw(0).windows(4).any(|w| w == b"boot"));
        assert!(cap.raw(1).starts_with(b"boot"));
        assert!(cap.raw(0).windows(2).any(|w| w == b"\x1b["));
    }

    #[test]
    fn filter_clear_view_and_rotation_gap() {
        let mut cap = Capture::new(4096);
        cap.ingest(0, 0, b"I (1) app: ready\n");
        cap.ingest(0, 0, b"E (2) net: fail\n");
        let only = cap.query(&Filter { level: Some('I'), tag: Some("app".into()), search: Some("ready".into()), ..Filter::default() }, false);
        assert_eq!(only.len(), 1);
        cap.clear_view();
        assert!(cap.query(&Filter::default(), true).is_empty());
        let kept = cap.raw(0);
        assert!(kept.windows(5).any(|w| w == b"ready"));

        let mut rotating = Capture::new(40);
        for i in 0..8 {
            rotating.ingest(0, i, format!("I ({i}) app: line{i}xxxx\n").as_bytes());
        }
        let export = rotating.export(&Filter::default(), false);
        assert!(!export.lossless);
        assert!(!export.gaps.is_empty());
        assert!(export.lines.len() < 8);
    }
}
