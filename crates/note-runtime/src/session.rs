//! One running instance on the replay backend: protocol requests, a bounded
//! log queue per client, and frames that resync to a full image (Spec §12).

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use note_machine::{Guest, ReplayMachine};
use note_protocol::frame::{self, FrameHeader, REFRESH_FULL};
use note_protocol::{Decoder, Kind, Message};
use serde_json::{json, Value};

use crate::logs::{Capture, Event, Filter};
use crate::network::{HostMode, NetworkInfo};

struct Client {
    applied_seq: u64,
    applied_epoch: u32,
    follow: bool,
    seen: u64,
    queue: VecDeque<Queued>,
    cap: usize,
    gaps: u64,
    pending_frame: Option<Vec<u8>>,
    /// `display.subscribe`: push a full frame whenever the display changes.
    push: bool,
    pushed: (u32, u64),
}

#[derive(Clone, Debug)]
pub enum Queued {
    Line { seq: u64, channel: u8, text: String },
    Gap { dropped: u64 },
}

pub struct Instance<G: Guest> {
    pub guest: G,
    logs: Capture,
    image: Vec<u8>,
    frame_seq: u64,
    epoch: u32,
    clients: Vec<(u64, Client)>,
    next_client: u64,
    pacing_revision: u64,
    queue_cap: usize,
    running: bool,
    network: NetworkInfo,
    avd: String,
    instance_id: String,
    /// Where `snapshot.*` keeps named snapshots (the AVD's `snapshots/`). None: unsupported.
    snapshot_dir: Option<std::path::PathBuf>,
    /// Developer endpoints this instance serves (reported by `status`).
    serial_port: Option<u16>,
    gdb_port: Option<u16>,
    /// Raw console bytes kept for a serial bridge (USB channel 1, UART0 channel 0) when tapped.
    tap: Option<[Vec<u8>; 2]>,
}

impl Instance<ReplayMachine> {
    pub fn from_trace(text: &str) -> Result<Instance<ReplayMachine>, String> {
        Ok(Instance::new(ReplayMachine::parse(text)?, 4096, 8))
    }
}

impl<G: Guest> Instance<G> {
    pub fn new(guest: G, log_limit: usize, queue_cap: usize) -> Instance<G> {
        let image = guest.frame().to_vec();
        let profile = guest.profile_id().to_string();
        let mut network = NetworkInfo::stopped(HostMode::Disabled);
        network.start(HostMode::Disabled, None, Vec::new(), "not-required");
        let _ = profile;
        Instance {
            image, guest, logs: Capture::new(log_limit), frame_seq: 1, epoch: 1,
            clients: Vec::new(), next_client: 1, pacing_revision: 0, queue_cap, running: true, network,
            avd: String::new(), instance_id: "replay".into(), snapshot_dir: None,
            serial_port: None, gdb_port: None, tap: None,
        }
    }

    pub fn set_identity(&mut self, avd: &str, instance: &str) {
        self.avd = avd.into();
        self.instance_id = instance.into();
    }

    /// Report the RFC 2217 serial port and GDB port in `status`; a serial bridge also taps the
    /// console (`take_tapped`), since the log capture consumes it.
    pub fn set_dev_ports(&mut self, serial: Option<u16>, gdb: Option<u16>) {
        self.serial_port = serial;
        self.gdb_port = gdb;
        self.tap = serial.map(|_| [Vec::new(), Vec::new()]);
    }

    /// Keep raw console bytes for `take_tapped` (a PTY bridge without an RFC 2217 port).
    pub fn tap_console(&mut self) {
        if self.tap.is_none() {
            self.tap = Some([Vec::new(), Vec::new()]);
        }
    }

    /// Console bytes since the last call, per channel (0 UART0, 1 USB), for a serial bridge.
    pub fn take_tapped(&mut self) -> [Vec<u8>; 2] {
        match &mut self.tap {
            Some(t) => [std::mem::take(&mut t[0]), std::mem::take(&mut t[1])],
            None => [Vec::new(), Vec::new()],
        }
    }

    pub fn set_snapshot_dir(&mut self, dir: std::path::PathBuf) {
        self.snapshot_dir = Some(dir);
    }

    /// `stop` clears this. Pause does not: the host keeps polling the socket.
    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn set_network(&mut self, network: NetworkInfo) {
        self.network = network;
    }

    pub fn attach(&mut self) -> u64 {
        let id = self.next_client;
        self.next_client += 1;
        self.clients.push((id, Client {
            applied_seq: 0, applied_epoch: self.epoch, follow: true, seen: 0,
            queue: VecDeque::new(), cap: self.queue_cap, gaps: 0, pending_frame: None,
            push: false, pushed: (0, 0),
        }));
        self.fanout();
        id
    }

    /// Release all per-connection queues and frame state, including after protocol errors.
    pub fn detach(&mut self, id: u64) {
        self.clients.retain(|(client, _)| *client != id);
    }

    /// Changes when the host must reset its real-time clock baseline.
    pub fn pacing_revision(&self) -> u64 {
        self.pacing_revision
    }

    pub fn queued(&self, id: u64) -> usize {
        self.clients.iter().find(|(c, _)| *c == id).map(|(_, c)| c.queue.len()).unwrap_or(0)
    }

    pub fn gaps(&self, id: u64) -> u64 {
        self.clients.iter().find(|(c, _)| *c == id).map(|(_, c)| c.gaps).unwrap_or(0)
    }

    pub fn advance(&mut self, target_ns: u64) {
        if self.guest.paused() || !self.running {
            return;
        }
        self.guest.advance(target_ns);
        self.pull();
    }

    pub fn push_console(&mut self, channel: u8, bytes: &[u8]) {
        self.logs.ingest(channel, self.guest.now_ns(), bytes);
        self.fanout();
    }

    pub fn request(&mut self, client: u64, payload: &str) -> Vec<Message> {
        let value: Value = match serde_json::from_str(payload) {
            Ok(v) => v,
            Err(err) => return vec![self.fail(0, "BadRequest", &err.to_string())],
        };
        let id = value.get("id").and_then(|v| v.as_u64()).unwrap_or(1);
        let method = value.get("method").and_then(|v| v.as_str()).unwrap_or("");
        match method {
            "hello" => vec![self.ok(id, json!({
                "protocol": 1,
                "runtime": "note-runtime/0.1.0",
                "engine": self.guest.engine(),
                "profile": { "id": self.guest.profile_id() },
                "avd": self.avd,
                "instance": self.instance_id,
                "epoch": self.epoch,
                "capabilities": ["display", "buttons", "logs", "replay"]
            }))],
            "status" => vec![self.ok(id, json!({
                "running": self.running,
                "paused": self.guest.paused(),
                "epoch": self.epoch,
                "profile": self.guest.profile_id(),
                "virtual_ns": self.guest.now_ns(),
                "battery_mv": self.guest.battery_mv(),
                "frame_seq": self.frame_seq,
                "serial_url": self.serial_port.map(|p| format!("rfc2217://127.0.0.1:{p}")),
                "gdb": self.gdb_port.map(|p| format!("127.0.0.1:{p}")),
                "usb_cable": self.guest.usb().0,
                "usb_host": self.guest.usb().1,
                "mic_listening": self.guest.mic_listening(),
                "leds": self.guest.leds().into_iter().map(|(name, lit)| json!({ "led": name, "lit": lit })).collect::<Vec<_>>()
            }))],
            "pause" => { self.guest.set_paused(true); vec![self.ok(id, json!({}))] }
            "resume" => {
                if self.guest.paused() {
                    self.pacing_revision = self.pacing_revision.wrapping_add(1);
                }
                self.guest.set_paused(false);
                vec![self.ok(id, json!({}))]
            }
            "stop" => { self.running = false; vec![self.ok(id, json!({}))] }
            "button.down" | "button.up" => self.button(id, &value, method.ends_with("down")),
            "battery.get" => vec![self.ok(id, json!({ "mv": self.guest.battery_mv() }))],
            "battery.set" => {
                let mv = value.get("mv").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                self.guest.set_battery_mv(mv);
                vec![self.ok(id, json!({ "mv": mv }))]
            }
            "network.info" => vec![self.ok(id, self.network.report())],
            "network.configure" => {
                let mode = match value.get("mode").and_then(|v| v.as_str()).unwrap_or("disabled") {
                    "user" => HostMode::User,
                    "setup" => HostMode::Setup,
                    "shared" => HostMode::Shared,
                    _ => HostMode::Disabled,
                };
                self.network.configure(mode);
                vec![self.ok(id, json!({ "restart_required": self.network.restart_required, "browser_url": self.network.reported_url() }))]
            }
            "display.get" | "screenshot.capture" => self.display_get(client, id),
            "snapshot.save" | "snapshot.load" | "snapshot.list" | "snapshot.delete" => self.snapshot(id, method, &value),
            "boot.reset" => {
                // EN reset; `mode` "download" holds GPIO0 low (ROM download mode), else SPI boot.
                let download = value.get("mode").and_then(|v| v.as_str()) == Some("download");
                match self.guest.pin_reset(download) {
                    Ok(()) => {
                        self.pull();
                        vec![self.ok(id, json!({ "mode": if download { "download" } else { "normal" } }))]
                    }
                    Err(err) => vec![self.fail(id, "Unsupported", &err)],
                }
            }
            "power.usb" => {
                // `cable`: supply + charger; `host`: USB-Serial/JTAG host (console). Either may be absent.
                let cable = value.get("cable").and_then(|v| v.as_bool());
                let host = value.get("host").and_then(|v| v.as_bool());
                self.guest.set_usb(cable, host);
                let (c, h) = self.guest.usb();
                vec![self.ok(id, json!({ "cable": c, "host": h }))]
            }
            "audio.mic" => {
                // Live: `pcm` (base64 LE i16 mono) at `rate`, resampled to the guest and dropped
                // while its receiver is off. File: `path`, a PCM WAV on this Mac at the guest's
                // microphone rate, queued from now on (the protocol is local to one user).
                if let Some(pcm) = value.get("pcm").and_then(|v| v.as_str()) {
                    let rate = value.get("rate").and_then(|v| v.as_u64()).unwrap_or(16_000) as u32;
                    let Some(bytes) = decode_base64(pcm) else {
                        return vec![self.fail(id, "BadRequest", "pcm is not base64")];
                    };
                    let samples: Vec<i16> = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                    return match self.guest.mic_pcm(&samples, rate) {
                        Ok(queued) => vec![self.ok(id, json!({ "samples": queued }))],
                        Err(err) => vec![self.fail(id, "Unsupported", &err)],
                    };
                }
                let Some(path) = value.get("path").and_then(|v| v.as_str()) else {
                    return vec![self.fail(id, "BadRequest", "audio.mic needs pcm or path")];
                };
                match std::fs::read(path).map_err(|e| format!("{path}: {e}")).and_then(|wav| self.guest.mic_wav(&wav)) {
                    Ok(samples) => vec![self.ok(id, json!({ "samples": samples }))],
                    Err(err) => vec![self.fail(id, "BadRequest", &err)],
                }
            }
            "audio.get" => {
                // Speaker PCM since `from` (absent: start now). At most two seconds come back, so
                // a slow client skips ahead instead of queueing an ever longer delay. The Audio
                // message (LE i16 mono) goes before the response.
                let from = value.get("from").and_then(|v| v.as_u64());
                let (rate, start, samples) = self.guest.speaker(from, 2 * 48_000);
                let mut out = Vec::new();
                if !samples.is_empty() {
                    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                    out.push(Message::new(Kind::Audio, id, bytes));
                }
                let next = start + samples.len() as u64;
                out.push(self.ok(id, json!({ "rate": rate, "start": start, "next": next, "count": samples.len() })));
                out
            }
            "display.ack" => {
                let seq = value.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
                if let Some((_, c)) = self.clients.iter_mut().find(|(cid, _)| *cid == client) {
                    if seq <= self.frame_seq {
                        c.applied_seq = seq;
                        c.applied_epoch = self.epoch;
                    }
                }
                vec![self.ok(id, json!({}))]
            }
            "logs.export" => {
                let filter = filter_of(&value);
                let view = value.get("view").and_then(|v| v.as_bool()).unwrap_or(true);
                let export = self.logs.export(&filter, view);
                vec![self.ok(id, json!({
                    "lossless": export.lossless,
                    "gaps": export.gaps.iter().map(|g| json!({ "channel": g.channel, "dropped_bytes": g.dropped_bytes })).collect::<Vec<_>>(),
                    "from_seq": export.from_seq,
                    "to_seq": export.to_seq,
                    "lines": export.lines.iter().map(|l| json!({
                        "seqs": l.seqs, "channels": l.channels, "text": l.text,
                        "level": l.level.map(|c| c.to_string()), "tag": l.tag
                    })).collect::<Vec<_>>()
                }))]
            }
            "logs.clear_view" => {
                self.logs.clear_view();
                vec![self.ok(id, json!({}))]
            }
            "display.subscribe" => {
                // Pushed frames (request id 0): a full frame now and after every change.
                let on = value.get("on").and_then(|v| v.as_bool()).unwrap_or(true);
                if let Some((_, c)) = self.clients.iter_mut().find(|(cid, _)| *cid == client) {
                    c.push = on;
                    c.pushed = (0, 0);
                }
                vec![self.ok(id, json!({ "subscribed": on }))]
            }
            "logs.follow" => {
                let on = value.get("follow").and_then(|v| v.as_bool()).unwrap_or(true);
                if let Some((_, c)) = self.clients.iter_mut().find(|(cid, _)| *cid == client) {
                    c.follow = on;
                }
                self.fanout();
                vec![self.ok(id, json!({ "follow": on }))]
            }
            "console.write" => {
                let channel = match value.get("channel").and_then(|v| v.as_str()).unwrap_or("uart0") {
                    "usb" => 1,
                    _ => 0,
                };
                let text = value.get("text").and_then(|v| v.as_str()).unwrap_or("");
                self.guest.console_input(channel, text.as_bytes());
                vec![self.ok(id, json!({ "bytes": text.len() }))]
            }
            other => vec![self.fail(id, "BadRequest", &format!("unknown method {other}"))],
        }
    }

    pub fn drain(&mut self, client: u64) -> Vec<Queued> {
        self.clients.iter_mut().find(|(id, _)| *id == client).map(|(_, c)| c.queue.drain(..).collect()).unwrap_or_default()
    }

    /// For a subscribed client, a full frame if the display changed since the last push.
    pub fn push_frame(&mut self, client: u64) -> Option<Vec<u8>> {
        let now = (self.epoch, self.frame_seq);
        let c = self.clients.iter_mut().find(|(id, _)| *id == client).map(|(_, c)| c)?;
        if !c.push || c.pushed == now {
            return None;
        }
        c.pushed = now;
        c.applied_seq = now.1;
        c.applied_epoch = now.0;
        Some(self.encode_frame(0))
    }

    pub fn take_frame(&mut self, client: u64) -> Option<Vec<u8>> {
        self.clients.iter_mut().find(|(id, _)| *id == client).and_then(|(_, c)| c.pending_frame.take())
    }

    fn button(&mut self, id: u64, value: &Value, down: bool) -> Vec<Message> {
        let name = value.get("button").and_then(|v| v.as_str()).unwrap_or("");
        match self.guest.button(name, down) {
            Ok(()) => { self.pull(); vec![self.ok(id, json!({ "button": name, "down": down }))] }
            Err(err) => vec![self.fail(id, "BadRequest", &err)],
        }
    }

    /// Named full-machine snapshots in the AVD (G7). Save writes `<name>.snap` atomically;
    /// load restores it and starts a new frame epoch so every client redraws from a full frame.
    fn snapshot(&mut self, id: u64, method: &str, value: &Value) -> Vec<Message> {
        let Some(dir) = self.snapshot_dir.clone() else {
            return vec![self.fail(id, "Unsupported", "this instance has no snapshot storage (run an AVD)")];
        };
        let name = value.get("name").and_then(|v| v.as_str()).unwrap_or("default");
        if method != "snapshot.list" && !valid_snapshot_name(name) {
            return vec![self.fail(id, "BadRequest", "snapshot names are 1-64 of A-Z a-z 0-9 . _ -")];
        }
        let path = dir.join(format!("{name}.snap"));
        match method {
            "snapshot.save" => {
                let bytes = match self.guest.save_snapshot() {
                    Ok(bytes) => bytes,
                    Err(err) => return vec![self.fail(id, "SnapshotFailed", &err)],
                };
                if let Err(err) = std::fs::create_dir_all(&dir).map_err(|e| e.to_string())
                    .and_then(|()| note_machine::snapshot::publish(&path, &bytes).map_err(|e| e.to_string()))
                {
                    return vec![self.fail(id, "Io", &err)];
                }
                vec![self.ok(id, json!({ "name": name, "bytes": bytes.len(), "virtual_ns": self.guest.now_ns() }))]
            }
            "snapshot.load" => {
                let bytes = match std::fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(err) => return vec![self.fail(id, "NotFound", &format!("{name}: {err}"))],
                };
                match self.guest.restore_snapshot(&bytes) {
                    Ok(report) => {
                        self.epoch = self.epoch.wrapping_add(1);
                        self.pacing_revision = self.pacing_revision.wrapping_add(1);
                        self.image = self.guest.frame().to_vec();
                        self.frame_seq += 1;
                        self.pull();
                        vec![self.ok(id, json!({ "name": name, "epoch": self.epoch, "report": report }))]
                    }
                    Err(err) => vec![self.fail(id, "SnapshotRejected", &err)],
                }
            }
            "snapshot.delete" => match std::fs::remove_file(&path) {
                Ok(()) => vec![self.ok(id, json!({ "name": name }))],
                Err(err) => vec![self.fail(id, "NotFound", &format!("{name}: {err}"))],
            },
            _ => vec![self.ok(id, json!({ "snapshots": list_snapshots(&dir) }))],
        }
    }

    fn display_get(&mut self, client: u64, id: u64) -> Vec<Message> {
        let encoded = self.encode_frame(0);
        if let Some((_, c)) = self.clients.iter_mut().find(|(cid, _)| *cid == client) {
            c.applied_seq = self.frame_seq;
            c.applied_epoch = self.epoch;
            c.pending_frame = None;
        }
        let (header, pixels) = frame::decode(&encoded).expect("fresh frame");
        vec![
            self.ok(id, json!({
                "seq": header.seq, "base_seq": header.base_seq, "full": true,
                "width": header.width, "height": header.height, "epoch": header.epoch,
                "bytes": pixels.len()
            })),
            Message::new(Kind::Frame, id, encoded),
        ]
    }

    fn pull(&mut self) {
        let now = self.guest.now_ns();
        for (ch, bytes) in self.guest.take_console() {
            if let Some(t) = &mut self.tap {
                if (ch as usize) < 2 { t[ch as usize].extend_from_slice(&bytes); }
            }
            self.logs.ingest(ch, now, &bytes);
        }
        let frame = self.guest.frame().to_vec();
        if frame != self.image {
            self.image = frame;
            self.frame_seq += 1;
        }
        self.fanout();
    }

    fn fanout(&mut self) {
        let seq = self.frame_seq;
        let epoch = self.epoch;
        let events: Vec<Event> = self.logs.events_after(0).cloned().collect();
        let bases: Vec<u64> = self.clients.iter().map(|(_, client)| {
            let full = client.applied_seq == 0 || client.applied_epoch != epoch || client.applied_seq + 1 != seq;
            if full { 0 } else { client.applied_seq }
        }).collect();
        let frames: Vec<Vec<u8>> = bases.into_iter().map(|base| self.encode_frame(base)).collect();
        for ((_, client), frame) in self.clients.iter_mut().zip(frames) {
            if client.follow {
                let fresh: Vec<&Event> = events.iter().filter(|e| e.seq > client.seen).collect();
                if fresh.len() > client.cap {
                    let keep = client.cap.saturating_sub(1);
                    let dropped = fresh.len() - keep;
                    client.gaps += dropped as u64;
                    client.queue.clear();
                    client.queue.push_back(Queued::Gap { dropped: dropped as u64 });
                    for ev in fresh.into_iter().skip(dropped) {
                        client.queue.push_back(line_of(ev));
                    }
                } else {
                    for ev in fresh {
                        if client.queue.len() == client.cap {
                            client.queue.pop_front();
                            client.gaps += 1;
                        }
                        client.queue.push_back(line_of(ev));
                    }
                }
                if let Some(last) = events.last() {
                    client.seen = last.seq;
                }
            }
            client.pending_frame = Some(frame);
        }
    }

    fn encode_frame(&self, base_seq: u64) -> Vec<u8> {
        self_encode(self, base_seq)
    }

    fn ok(&self, id: u64, mut extra: Value) -> Message {
        if let Some(obj) = extra.as_object_mut() {
            obj.insert("ok".into(), json!(true));
        }
        Message::new(Kind::Response, id, serde_json::to_vec(&extra).unwrap_or_else(|_| b"{\"ok\":true}".to_vec()))
    }

    fn fail(&self, id: u64, code: &str, message: &str) -> Message {
        Message::new(Kind::Response, id, serde_json::to_vec(&json!({
            "ok": false, "error": { "code": code, "message": message }
        })).unwrap())
    }
}

fn line_of(ev: &Event) -> Queued {
    Queued::Line { seq: ev.seq, channel: ev.channel, text: ev.text.clone() }
}

fn self_encode<G: Guest>(inst: &Instance<G>, base_seq: u64) -> Vec<u8> {
    let header = FrameHeader {
        instance_hash: 1,
        epoch: inst.epoch,
        seq: inst.frame_seq,
        base_seq,
        virtual_ns: inst.guest.now_ns(),
        host_ns: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0),
        width: inst.guest.width(),
        height: inst.guest.height(),
        format: inst.guest.format(),
        palette_id: 0,
        source: inst.guest.frame_source(),
        refresh: REFRESH_FULL,
        dirty_x: 0,
        dirty_y: 0,
        dirty_w: inst.guest.width(),
        dirty_h: inst.guest.height(),
        pixel_hash: frame::fnv1a(&inst.image),
    };
    frame::encode(&header, &inst.image).expect("frame matches its geometry")
}

fn filter_of(value: &Value) -> Filter {
    Filter {
        channel: match value.get("source").and_then(|v| v.as_str()) {
            Some("uart0") => Some(0),
            Some("usb") => Some(1),
            _ => None,
        },
        level: value.get("level").and_then(|v| v.as_str()).and_then(|s| s.chars().next()),
        tag: value.get("tag").and_then(|v| v.as_str()).map(str::to_string),
        search: value.get("search").and_then(|v| v.as_str()).map(str::to_string),
    }
}

/// Serve one connection until it closes. Requests are answered under the instance lock.
pub fn serve_connection<G: Guest>(inst: Arc<Mutex<Instance<G>>>, mut sock: UnixStream) {
    let id = inst.lock().expect("instance").attach();
    struct Attached<G: Guest>(Arc<Mutex<Instance<G>>>, u64);
    impl<G: Guest> Drop for Attached<G> {
        fn drop(&mut self) {
            if let Ok(mut inst) = self.0.lock() { inst.detach(self.1); }
        }
    }
    let _attached = Attached(Arc::clone(&inst), id);
    let mut dec = Decoder::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = match sock.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        dec.feed(&buf[..n]);
        loop {
            let msg = match dec.next_message() {
                Ok(Some(msg)) => msg,
                Ok(None) => break,
                Err(_) => return,
            };
            if msg.kind != Kind::Request {
                continue;
            }
            let text = String::from_utf8_lossy(&msg.payload);
            let mut replies = inst.lock().expect("instance").request(id, &text);
            for reply in &mut replies {
                reply.request_id = msg.request_id;
            }
            for reply in replies {
                if sock.write_all(&reply.encode().unwrap_or_default()).is_err() {
                    return;
                }
            }
        }
    }
}

/// Standard base64 (padding optional); None on any other byte.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let text = text.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    for chunk in text.chunks(4) {
        let mut acc = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            acc |= value(c)? << (18 - 6 * i);
        }
        let bytes = acc.to_be_bytes();
        match chunk.len() {
            4 => out.extend_from_slice(&bytes[1..4]),
            3 => out.extend_from_slice(&bytes[1..3]),
            2 => out.push(bytes[1]),
            _ => return None,
        }
    }
    Some(out)
}

/// One request over a fresh connection: every message up to and including the response, and
/// for a reply that announces a full frame (`display.get`) the frame that follows it. Reads
/// until those are complete (a large frame arrives in several reads) or 5 s pass.
pub fn transact(path: &Path, request_id: u64, payload: &[u8]) -> std::io::Result<Vec<Message>> {
    let mut sock = UnixStream::connect(path)?;
    sock.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    let bytes = Message::new(Kind::Request, request_id, payload.to_vec()).encode().map_err(|e| std::io::Error::other(e.to_string()))?;
    sock.write_all(&bytes)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut buf = vec![0u8; 256 * 1024];
    let mut dec = Decoder::new();
    let mut out: Vec<Message> = Vec::new();
    let mut want_frame = false;
    let mut have_response = false;
    loop {
        loop {
            match dec.next_message() {
                Ok(Some(msg)) => {
                    if msg.kind == Kind::Response && msg.request_id == request_id {
                        have_response = true;
                        want_frame = String::from_utf8_lossy(&msg.payload).contains("\"full\":true");
                    }
                    if msg.kind == Kind::Frame {
                        want_frame = false;
                    }
                    out.push(msg);
                }
                Ok(None) => break,
                Err(err) => return Err(std::io::Error::other(err.to_string())),
            }
        }
        if have_response && !want_frame {
            return Ok(out);
        }
        if std::time::Instant::now() >= deadline {
            return if have_response { Ok(out) } else { Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "no response")) };
        }
        match sock.read(&mut buf) {
            Ok(0) => return if have_response { Ok(out) } else { Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "closed")) },
            Ok(n) => dec.feed(&buf[..n]),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return if have_response { Ok(out) } else { Err(err) },
        }
    }
}

struct Conn {
    id: u64,
    sock: UnixStream,
    dec: Decoder,
    /// Bytes still to send. The socket is nonblocking, so a full frame may
    /// take more than one poll. Capped so a client that stops reading cannot
    /// grow without limit.
    outbuf: Vec<u8>,
}

/// Control socket pumped on the emulation thread. `NoteMachine` is not `Send`,
/// so connections are not handed to other threads.
pub struct OwnedServer<G: Guest> {
    pub inst: Instance<G>,
    listener: UnixListener,
    conns: Vec<Conn>,
}

impl<G: Guest> OwnedServer<G> {
    pub fn bind(inst: Instance<G>, path: &Path) -> std::io::Result<OwnedServer<G>> {
        let listener = bind_control(path)?;
        listener.set_nonblocking(true)?;
        Ok(OwnedServer { inst, listener, conns: Vec::new() })
    }

    pub fn poll(&mut self) -> std::io::Result<()> {
        loop {
            match self.listener.accept() {
                Ok((sock, _)) => {
                    sock.set_nonblocking(true)?;
                    let id = self.inst.attach();
                    self.conns.push(Conn { id, sock, dec: Decoder::new(), outbuf: Vec::new() });
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                // A client that gave up between connect and accept is not a server failure.
                Err(err) if matches!(err.kind(), std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::Interrupted) => continue,
                Err(err) => return Err(err),
            }
        }
        let mut dead = Vec::new();
        let mut pending = Vec::new();
        for (index, conn) in self.conns.iter_mut().enumerate() {
            let mut buf = [0u8; 8192];
            loop {
                match conn.sock.read(&mut buf) {
                    Ok(0) => {
                        dead.push(index);
                        break;
                    }
                    Ok(n) => conn.dec.feed(&buf[..n]),
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        dead.push(index);
                        break;
                    }
                }
            }
            loop {
                match conn.dec.next_message() {
                    Ok(Some(msg)) if msg.kind == Kind::Request => {
                        pending.push((index, conn.id, msg.request_id, String::from_utf8_lossy(&msg.payload).into_owned()));
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(_) => {
                        dead.push(index);
                        break;
                    }
                }
            }
        }
        let mut outgoing = Vec::new();
        for (index, id, request_id, text) in pending {
            let mut replies = self.inst.request(id, &text);
            for reply in &mut replies {
                reply.request_id = request_id;
                if let Ok(bytes) = reply.encode() {
                    outgoing.push((index, bytes));
                }
            }
        }
        // Pushed frames for subscribers whose socket is keeping up (a slow one skips states,
        // it never queues them).
        for (index, conn) in self.conns.iter().enumerate() {
            if conn.outbuf.len() < 256 * 1024 {
                if let Some(frame) = self.inst.push_frame(conn.id) {
                    outgoing.push((index, Message::new(Kind::Frame, 0, frame).encode().unwrap_or_default()));
                }
            }
        }
        const OUT_CAP: usize = 1024 * 1024;
        for (index, bytes) in outgoing {
            if index >= self.conns.len() { continue; }
            if self.conns[index].outbuf.len().saturating_add(bytes.len()) > OUT_CAP {
                dead.push(index);
                continue;
            }
            self.conns[index].outbuf.extend_from_slice(&bytes);
        }
        dead.extend(self.flush_writes());
        dead.sort_unstable();
        dead.dedup();
        for index in dead.into_iter().rev() {
            if index < self.conns.len() {
                let conn = self.conns.swap_remove(index);
                self.inst.detach(conn.id);
            }
        }
        Ok(())
    }

    /// Push queued bytes. `WouldBlock` leaves the rest for the next poll.
    fn flush_writes(&mut self) -> Vec<usize> {
        let mut dead = Vec::new();
        for (index, conn) in self.conns.iter_mut().enumerate() {
            while !conn.outbuf.is_empty() {
                match conn.sock.write(&conn.outbuf) {
                    Ok(0) => {
                        dead.push(index);
                        break;
                    }
                    Ok(n) => {
                        conn.outbuf.drain(..n);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock || err.kind() == std::io::ErrorKind::Interrupted => break,
                    Err(_) => {
                        dead.push(index);
                        break;
                    }
                }
            }
        }
        dead
    }
}

pub fn bind_control(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    listener.set_nonblocking(false)?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_padded_and_unpadded() {
        assert_eq!(decode_base64("AAEC/w==").unwrap(), vec![0, 1, 2, 255]);
        assert_eq!(decode_base64("AAEC/w").unwrap(), vec![0, 1, 2, 255]);
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert!(decode_base64("a*b=").is_none());
    }
    use note_protocol::frame::{self, FrameError};
    use std::time::Duration;

    const TRACE: &str = include_str!("../../../fixtures/traces/note4-synthetic.jsonl");

    fn instance() -> Instance<ReplayMachine> {
        Instance::from_trace(TRACE).unwrap()
    }

    #[test]
    fn pause_keeps_the_instance_running_and_stop_ends_it() {
        let mut inst = instance();
        let client = inst.attach();
        inst.request(client, r#"{"method":"pause"}"#);
        assert!(inst.guest.paused());
        assert!(inst.is_running());
        inst.request(client, r#"{"method":"resume"}"#);
        assert!(!inst.guest.paused());
        inst.request(client, r#"{"method":"stop"}"#);
        assert!(!inst.is_running());
    }

    fn json_of(msg: &Message) -> Value {
        serde_json::from_slice(&msg.payload).unwrap()
    }

    #[test]
    fn a_subscriber_gets_one_full_frame_per_display_change() {
        let mut inst = instance();
        let client = inst.attach();
        assert!(inst.push_frame(client).is_none(), "not subscribed");
        inst.request(client, r#"{"id":1,"method":"display.subscribe"}"#);
        let first = inst.push_frame(client).expect("current frame on subscribe");
        let (header, _) = frame::decode(&first).unwrap();
        assert_eq!(header.base_seq, 0, "pushed frames are full");
        assert!(inst.push_frame(client).is_none(), "nothing changed");
        let seq = inst.frame_seq;
        let mut t = 0;
        while inst.frame_seq == seq && t < 60_000_000_000 {
            t += 100_000_000;
            inst.advance(t);
        }
        assert!(inst.frame_seq > seq, "the trace changes the display");
        assert!(inst.push_frame(client).is_some(), "a change is pushed");
        inst.request(client, r#"{"id":2,"method":"display.subscribe","on":false}"#);
        inst.epoch += 1;
        assert!(inst.push_frame(client).is_none(), "unsubscribed");
    }

    #[test]
    fn snapshot_requests_need_storage_an_engine_and_a_safe_name() {
        let mut inst = instance();
        let client = inst.attach();
        let reply = inst.request(client, r#"{"id":1,"method":"snapshot.save","name":"a"}"#);
        assert_eq!(json_of(&reply[0]).pointer("/error/code").unwrap(), "Unsupported");
        let dir = std::env::temp_dir().join(format!("snaps-{}", std::process::id()));
        inst.set_snapshot_dir(dir.clone());
        for bad in ["", "../x", ".hidden", "a/b", &"x".repeat(65)] {
            let req = json!({ "id": 2, "method": "snapshot.save", "name": bad }).to_string();
            assert_eq!(json_of(&inst.request(client, &req)[0]).pointer("/error/code").unwrap(), "BadRequest", "{bad:?}");
        }
        let reply = inst.request(client, r#"{"id":3,"method":"snapshot.save","name":"ok-1.v2"}"#);
        assert_eq!(json_of(&reply[0]).pointer("/error/code").unwrap(), "SnapshotFailed", "replay has no machine");
        let reply = inst.request(client, r#"{"id":4,"method":"snapshot.load","name":"missing"}"#);
        assert_eq!(json_of(&reply[0]).pointer("/error/code").unwrap(), "NotFound");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("old.snap"), b"1").unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(dir.join("new.snap"), b"22").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let reply = inst.request(client, r#"{"id":5,"method":"snapshot.list"}"#);
        let names: Vec<String> = json_of(&reply[0])["snapshots"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["new", "old"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconnect_gets_a_full_image_and_a_dropped_delta_resyncs() {
        let mut inst = instance();
        inst.advance(1_000_000);
        let client = inst.attach();
        let msgs = inst.request(client, r#"{"method":"display.get"}"#);
        let frame = frame::decode(&msgs[1].payload).unwrap();
        assert_eq!(frame.0.base_seq, 0);
        assert_eq!(frame.1, [0x01, 0x23, 0x45, 0x67]);
        inst.request(client, &format!(r#"{{"method":"display.ack","seq":{}}}"#, frame.0.seq));

        inst.request(client, r#"{"method":"button.down","button":"ok"}"#);
        let delta = frame::decode(&inst.take_frame(client).unwrap()).unwrap();
        assert_eq!(delta.0.base_seq, frame.0.seq);
        assert_eq!(delta.1, [0x89, 0xab, 0xcd, 0xef]);
        // The client never applied that delta. The next change must be a full image.
        inst.request(client, r#"{"method":"button.down","button":"up"}"#);
        let full = frame::decode(&inst.take_frame(client).unwrap()).unwrap();
        assert_eq!(full.0.base_seq, 0);
        assert_eq!(full.1, [0x00, 0x00, 0x11, 0x11]);
        let err = frame::apply(full.0.epoch, full.0.seq, &full.1, &delta.0, &delta.1).unwrap_err();
        assert!(matches!(err, FrameError::Unappliable { .. }));
        let repaired = frame::apply(1, 0, &[], &full.0, &full.1).unwrap();
        assert_eq!(repaired, [0x00, 0x00, 0x11, 0x11]);

        let late = inst.attach();
        let again = inst.request(late, r#"{"method":"display.get"}"#);
        let late_frame = frame::decode(&again[1].payload).unwrap();
        assert_eq!(late_frame.0.base_seq, 0);
        assert_eq!(late_frame.1, [0x00, 0x00, 0x11, 0x11]);
    }

    #[test]
    fn two_clients_share_state_and_a_slow_log_queue_stays_bounded() {
        let mut inst = Instance::new(ReplayMachine::parse(TRACE).unwrap(), 1_000_000, 2);
        let a = inst.attach();
        let b = inst.attach();
        for i in 0..20 {
            inst.push_console(0, format!("I ({i}) app: line{i}\n").as_bytes());
        }
        assert!(inst.queued(a) <= 2);
        assert!(inst.gaps(a) > 0);
        let response = inst.request(b, r#"{"method":"button.down","button":"ok"}"#);
        assert_eq!(json_of(&response[0])["ok"], true);
        let status = inst.request(a, r#"{"method":"status"}"#);
        assert_eq!(json_of(&status[0])["frame_seq"], json_of(&inst.request(b, r#"{"method":"status"}"#)[0])["frame_seq"]);
        assert!(inst.queued(a) <= 2);
        let _ = (a, b);
    }

    #[test]
    fn pausing_follow_keeps_the_capture_and_console_write_reaches_the_guest() {
        let mut inst = instance();
        let client = inst.attach();
        inst.request(client, r#"{"method":"logs.follow","follow":false}"#);
        inst.push_console(0, b"I (9) app: hidden\n");
        assert!(inst.drain(client).is_empty());
        inst.request(client, r#"{"method":"logs.follow","follow":true}"#);
        let queued = inst.drain(client);
        assert!(queued.iter().any(|item| matches!(item, Queued::Line { text, .. } if text == "hidden")));
        inst.request(client, r#"{"method":"console.write","channel":"uart0","text":"hi\n"}"#);
        assert_eq!(inst.guest.input(0), b"hi\n");
    }

    /// Boots the NOTE4 demo on esp32sim and checks the product session against it.
    /// `cargo test --release -p note-runtime --lib live_note4 -- --ignored`
    #[test]
    #[ignore]
    fn live_note4_serves_a_full_frame_and_names_the_engine() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let firmware = repo.join("third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin");
        assert!(firmware.exists(), "fixture missing: {}", firmware.display());
        let profile = note_core::profile::find(&note_core::profile::profiles_dir(), "note4").unwrap();
        let rom = std::fs::read(note_core::rom::installed().expect("run `ndb rom import`").path).unwrap();
        let image = note_core::flash::load(&firmware, (profile.flash_mib as usize) << 20).unwrap();
        let mut machine = note_machine::NoteMachine::new(
            &profile, &rom, &image, [0x02, 0x4e, 0x4f, 0x54, 0x45, 0x04], 1_700_000_000,
            &note_machine::RadioConfig::default(),
        ).unwrap();
        machine.set_battery_mv(3900);
        let mut inst = Instance::new(machine, 1 << 20, 8);
        inst.advance(400_000_000);
        let client = inst.attach();
        let hello = inst.request(client, r#"{"method":"hello","protocol":1}"#);
        assert_eq!(json_of(&hello[0])["engine"], "esp32sim");
        assert_eq!(json_of(&hello[0])["profile"]["id"], "note4");
        let msgs = inst.request(client, r#"{"method":"display.get"}"#);
        let (header, pixels) = frame::decode(&msgs[1].payload).unwrap();
        assert_eq!(header.base_seq, 0);
        assert_eq!((header.width, header.height), (400, 300));
        assert_eq!(pixels.len(), 60_000);
        let logs = inst.request(client, r#"{"method":"logs.export","view":false}"#);
        let lines = json_of(&logs[0])["lines"].as_array().cloned().unwrap_or_default();
        assert!(!lines.is_empty(), "ROM/app console should have reached the capture");
        let other = inst.attach();
        let a = inst.request(client, r#"{"method":"status"}"#);
        let b = inst.request(other, r#"{"method":"status"}"#);
        assert_eq!(json_of(&a[0])["frame_seq"], json_of(&b[0])["frame_seq"]);
        assert_eq!(json_of(&a[0])["battery_mv"], 3900);
    }

    #[test]
    fn disconnected_owned_clients_release_queues_and_frames() {
        let path = std::env::temp_dir().join(format!("note-churn-{}.sock", std::process::id()));
        let mut host = OwnedServer::bind(instance(), &path).unwrap();
        let survivor = UnixStream::connect(&path).unwrap();
        host.poll().unwrap();
        for _ in 0..20 {
            let client = UnixStream::connect(&path).unwrap();
            host.poll().unwrap();
            assert_eq!(host.inst.clients.len(), 2);
            drop(client);
            host.poll().unwrap();
            assert_eq!(host.inst.clients.len(), 1);
        }
        host.inst.push_console(0, b"I (1) app: still connected\n");
        assert!(host.inst.queued(1) > 0);
        drop(survivor);
        host.poll().unwrap();
        assert!(host.inst.clients.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn threaded_disconnect_and_bad_envelope_release_clients() {
        for bad in [false, true] {
            let inst = Arc::new(Mutex::new(instance()));
            let (mut client, server) = UnixStream::pair().unwrap();
            let shared = Arc::clone(&inst);
            let worker = std::thread::spawn(move || serve_connection(shared, server));
            if bad { client.write_all(&[0; 20]).unwrap(); }
            drop(client);
            worker.join().unwrap();
            assert!(inst.lock().unwrap().clients.is_empty());
        }
    }

    #[test]
    fn resume_resets_pacing_only_after_a_pause() {
        let mut inst = instance();
        let id = inst.attach();
        inst.request(id, r#"{"method":"resume"}"#);
        assert_eq!(inst.pacing_revision(), 0);
        inst.request(id, r#"{"method":"pause"}"#);
        inst.request(id, r#"{"method":"resume"}"#);
        assert_eq!(inst.pacing_revision(), 1);
    }

    #[test]
    fn two_unix_clients_both_complete_hello() {
        let path = std::env::temp_dir().join(format!("note-ctl-{}-{}.sock", std::process::id(), std::process::id()));
        let listener = bind_control(&path).unwrap();
        let inst = Arc::new(Mutex::new(instance()));
        let accept = {
            let inst = Arc::clone(&inst);
            std::thread::spawn(move || {
                for _ in 0..2 {
                    let (sock, _) = listener.accept().unwrap();
                    let inst = Arc::clone(&inst);
                    std::thread::spawn(move || serve_connection(inst, sock));
                }
            })
        };
        let mut socks = Vec::new();
        for _ in 0..2 {
            let mut sock = UnixStream::connect(&path).unwrap();
            sock.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let req = Message::new(Kind::Request, 7, br#"{"method":"hello","protocol":1}"#.to_vec()).encode().unwrap();
            sock.write_all(&req).unwrap();
            socks.push(sock);
        }
        for sock in &mut socks {
            let mut buf = [0u8; 1024];
            let n = sock.read(&mut buf).unwrap();
            let mut dec = Decoder::new();
            dec.feed(&buf[..n]);
            let msg = dec.next_message().unwrap().unwrap();
            assert_eq!(msg.request_id, 7);
            assert_eq!(json_of(&msg)["ok"], true);
            assert_eq!(json_of(&msg)["engine"], "replay");
        }
        accept.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }
}

pub fn valid_snapshot_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `<name>.snap` files in `dir`, newest first, with size and modification time (Unix seconds).
pub fn list_snapshots(dir: &std::path::Path) -> Vec<Value> {
    let mut out: Vec<(u64, Value)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_str()?.strip_suffix(".snap")?.to_string();
            let meta = e.metadata().ok()?;
            let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
            Some((mtime, json!({ "name": name, "bytes": meta.len(), "modified": mtime })))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, v)| v).collect()
}
