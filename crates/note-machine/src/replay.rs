//! Trace replay (proposal §10). The product track drives this instead of the chip.
//! A replay result never satisfies a native gate.
//!
//! The trace is JSON lines. Timed `console` and `frame` events play back as virtual
//! time advances. `on` lines are scripted responses to a button.

use std::collections::HashMap;

const WINDOW_NS: u64 = 500_000_000;

#[derive(Clone, Debug)]
struct Timed {
    t_ns: u64,
    channel: Option<u8>,
    text: Option<String>,
    frame: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
struct Script {
    button: String,
    down: bool,
    channel: u8,
    text: Option<String>,
    frame: Option<Vec<u8>>,
}

/// What the runtime needs from either a replay or, later, a live machine.
pub trait Guest {
    /// Configure the host station's password for the guest's protected setup hotspot.
    fn set_softap_passphrase(&mut self, _psk: &str) -> bool { false }
    fn profile_id(&self) -> &str;
    /// `"replay"` or `"esp32sim"`. Reported by `hello`.
    fn engine(&self) -> &'static str;
    fn advance(&mut self, target_ns: u64);
    fn now_ns(&self) -> u64;
    fn take_console(&mut self) -> Vec<(u8, Vec<u8>)>;
    fn button(&mut self, id: &str, down: bool) -> Result<(), String>;
    fn battery_mv(&self) -> u32;
    fn set_battery_mv(&mut self, mv: u32);
    fn frame(&self) -> &[u8];
    fn width(&self) -> u16;
    fn height(&self) -> u16;
    /// Protocol format byte: 1 pal2, 2 gray4.
    fn format(&self) -> u8;
    fn console_input(&mut self, channel: u8, bytes: &[u8]);
    fn input(&self, channel: u8) -> &[u8];
    fn set_paused(&mut self, paused: bool);
    fn paused(&self) -> bool;
    /// Board LEDs as (name, lit). Boards without LEDs report none.
    fn leds(&self) -> Vec<(&'static str, bool)> {
        Vec::new()
    }
    /// Speaker PCM (mono i16) from absolute sample index `from` (None: from now, no backlog),
    /// at most `max` samples ending at the newest: (rate, index of the first returned, samples).
    fn speaker(&self, _from: Option<u64>, _max: usize) -> (u32, u64, Vec<i16>) {
        (0, 0, Vec::new())
    }
    /// EN reset of the chip; `download` holds GPIO0 low (ROM download mode, esptool's target).
    fn pin_reset(&mut self, _download: bool) -> Result<(), String> {
        Err("reset needs the esp32sim engine".into())
    }
    /// Queue a PCM WAV as microphone input from now on; the guest reads it as it runs its I2S
    /// receiver, then hears silence again. Returns the queued sample count.
    fn mic_wav(&mut self, _wav: &[u8]) -> Result<usize, String> {
        Err("the microphone needs the esp32sim engine".into())
    }
    /// The guest is reading its microphone (an I2S receiver is running).
    fn mic_listening(&self) -> bool {
        false
    }
    /// Live microphone samples (mono i16 at `rate`) from the host. Dropped while the guest's
    /// receiver is stopped, so nothing stale is heard later. Returns the samples queued.
    fn mic_pcm(&mut self, _samples: &[i16], _rate: u32) -> Result<usize, String> {
        Err("the microphone needs the esp32sim engine".into())
    }
    /// USB cable: external supply and charger (`plugged`), and the USB host on USB-Serial/JTAG.
    fn set_usb(&mut self, _cable: Option<bool>, _host: Option<bool>) {}
    /// (cable plugged, host attached).
    fn usb(&self) -> (bool, bool) {
        (false, true)
    }
    /// Protocol frame source: 1 panel, 2 legacy console.
    fn frame_source(&self) -> u8 {
        1
    }
    /// Full machine snapshot (G7). Replay has no machine to snapshot.
    fn save_snapshot(&self) -> Result<Vec<u8>, String> {
        Err("snapshots need the esp32sim engine".into())
    }
    /// Restore one; returns a JSON report. The machine is unchanged on error.
    fn restore_snapshot(&mut self, _bytes: &[u8]) -> Result<serde_json::Value, String> {
        Err("snapshots need the esp32sim engine".into())
    }
}

pub struct ReplayMachine {
    profile: String,
    width: u16,
    height: u16,
    format: u8,
    timed: Vec<Timed>,
    scripts: Vec<Script>,
    cursor: usize,
    now_ns: u64,
    pending: Vec<(u8, Vec<u8>)>,
    image: Vec<u8>,
    battery_mv: u32,
    paused: bool,
    buttons: HashMap<String, bool>,
    input: [Vec<u8>; 2],
}

impl ReplayMachine {
    pub fn parse(text: &str) -> Result<ReplayMachine, String> {
        let mut profile = "note4".to_string();
        let mut width = 4u16;
        let mut height = 2u16;
        let mut format = 2u8;
        let mut timed = Vec::new();
        let mut scripts = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(line).map_err(|e| format!("line {}: {e}", n + 1))?;
            let op = v.get("op").and_then(|s| s.as_str()).ok_or_else(|| format!("line {}: missing op", n + 1))?;
            match op {
                "meta" => {
                    if let Some(id) = v.get("profile").and_then(|s| s.as_str()) {
                        profile = id.to_string();
                    }
                    width = v.get("w").and_then(|s| s.as_u64()).unwrap_or(width as u64) as u16;
                    height = v.get("h").and_then(|s| s.as_u64()).unwrap_or(height as u64) as u16;
                    format = match v.get("format").and_then(|s| s.as_str()).unwrap_or("gray4") {
                        "pal2" => 1,
                        "gray4" => 2,
                        other => return Err(format!("line {}: format {other}", n + 1)),
                    };
                }
                "console" => timed.push(Timed {
                    t_ns: v.get("t_ns").and_then(|s| s.as_u64()).unwrap_or(0),
                    channel: Some(channel_of(v.get("ch").and_then(|s| s.as_str()).unwrap_or("uart0"))?),
                    text: Some(text_of(&v)?),
                    frame: None,
                }),
                "frame" => timed.push(Timed {
                    t_ns: v.get("t_ns").and_then(|s| s.as_u64()).unwrap_or(0),
                    channel: None,
                    text: None,
                    frame: Some(hex_of(&v)?),
                }),
                "on" => scripts.push(Script {
                    button: v.get("button").and_then(|s| s.as_str()).unwrap_or("ok").to_string(),
                    down: v.get("down").and_then(|s| s.as_bool()).unwrap_or(true),
                    channel: channel_of(v.get("ch").and_then(|s| s.as_str()).unwrap_or("uart0"))?,
                    text: v.get("text").and_then(|s| s.as_str()).map(str::to_string),
                    frame: match v.get("hex") {
                        Some(_) => Some(hex_of(&v)?),
                        None => None,
                    },
                }),
                other => return Err(format!("line {}: unknown op {other}", n + 1)),
            }
        }
        timed.sort_by_key(|t| t.t_ns);
        let pixels = match format {
            1 => width as usize * height as usize / 4,
            _ => width as usize * height as usize / 2,
        };
        Ok(ReplayMachine {
            profile, width, height, format, timed, scripts, cursor: 0, now_ns: 0,
            pending: Vec::new(), image: vec![0; pixels], battery_mv: 3900, paused: false,
            buttons: HashMap::new(), input: [Vec::new(), Vec::new()],
        })
    }

    pub fn load(path: &std::path::Path) -> Result<ReplayMachine, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        Self::parse(&text)
    }
}

impl Guest for ReplayMachine {
    fn profile_id(&self) -> &str { &self.profile }
    fn engine(&self) -> &'static str { "replay" }
    fn now_ns(&self) -> u64 { self.now_ns }
    fn frame(&self) -> &[u8] { &self.image }
    fn width(&self) -> u16 { self.width }
    fn height(&self) -> u16 { self.height }
    fn format(&self) -> u8 { self.format }
    fn battery_mv(&self) -> u32 { self.battery_mv }
    fn set_battery_mv(&mut self, mv: u32) { self.battery_mv = mv; }
    fn paused(&self) -> bool { self.paused }
    fn set_paused(&mut self, paused: bool) { self.paused = paused; }
    fn input(&self, channel: u8) -> &[u8] { &self.input[channel as usize] }

    fn advance(&mut self, target_ns: u64) {
        if self.paused || target_ns < self.now_ns {
            return;
        }
        while self.cursor < self.timed.len() && self.timed[self.cursor].t_ns <= target_ns {
            let ev = self.timed[self.cursor].clone();
            self.cursor += 1;
            if let (Some(ch), Some(text)) = (ev.channel, ev.text) {
                self.pending.push((ch, text.into_bytes()));
            }
            if let Some(frame) = ev.frame {
                self.image = frame;
            }
        }
        self.now_ns = target_ns;
    }

    fn take_console(&mut self) -> Vec<(u8, Vec<u8>)> {
        std::mem::take(&mut self.pending)
    }

    fn button(&mut self, id: &str, down: bool) -> Result<(), String> {
        if id.is_empty() {
            return Err("missing button".into());
        }
        self.buttons.insert(id.to_string(), down);
        if let Some(script) = self.scripts.iter().find(|s| s.button == id && s.down == down).cloned() {
            if let Some(text) = script.text {
                self.pending.push((script.channel, text.into_bytes()));
            }
            if let Some(frame) = script.frame {
                self.image = frame;
            }
        }
        Ok(())
    }

    fn console_input(&mut self, channel: u8, bytes: &[u8]) {
        if (channel as usize) < self.input.len() {
            self.input[channel as usize].extend_from_slice(bytes);
        }
    }
}

fn channel_of(name: &str) -> Result<u8, String> {
    match name {
        "uart0" => Ok(0),
        "usb" => Ok(1),
        other => Err(format!("unknown channel {other}")),
    }
}

fn text_of(v: &serde_json::Value) -> Result<String, String> {
    v.get("text").and_then(|s| s.as_str()).map(str::to_string).ok_or_else(|| "console line needs text".into())
}

fn hex_of(v: &serde_json::Value) -> Result<Vec<u8>, String> {
    let hex = v.get("hex").and_then(|s| s.as_str()).ok_or_else(|| "frame needs hex".to_string())?;
    if hex.len() % 2 != 0 {
        return Err("odd hex length".into());
    }
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| e.to_string())).collect()
}

/// Cross-channel window used by the log merge. Exposed so the runtime and the
/// replay traces share one number.
pub const TWIN_WINDOW_NS: u64 = WINDOW_NS;

#[cfg(test)]
mod tests {
    use super::*;

    const TRACE: &str = include_str!("../../../fixtures/traces/note4-synthetic.jsonl");

    #[test]
    fn playback_then_a_button_changes_the_frame() {
        let mut m = ReplayMachine::parse(TRACE).unwrap();
        m.advance(0);
        let boot = m.take_console();
        assert!(boot.iter().any(|(ch, b)| *ch == 0 && b == b"boot\n"));
        m.advance(1_000_000);
        let ready = m.take_console();
        assert_eq!(ready.iter().filter(|(ch, b)| *ch == 0 && b.starts_with(b"I (1)")).count(), 2);
        assert_eq!(m.frame(), &[0x01, 0x23, 0x45, 0x67]);
        m.button("ok", true).unwrap();
        assert_eq!(m.frame(), &[0x89, 0xab, 0xcd, 0xef]);
        let line = m.take_console();
        assert_eq!(line[0].1, b"I (2) btn: ok\n");
        m.set_paused(true);
        let at = m.now_ns();
        m.advance(at + 50);
        assert_eq!(m.now_ns(), at);
    }
}
