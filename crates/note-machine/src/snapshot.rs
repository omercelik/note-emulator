//! Machine snapshots (Spec §14, G7), schema 2: the whole machine.
//!
//! Save only between `Machine::run` calls. `run` retires whole instructions, and its
//! mid-quantum `resume_at` does not outlive the call, so both cores are between
//! instructions — a scheduling-quantum boundary. A save while `NoteMachine` is inside
//! a slice is refused.
//!
//! Sections: `MACH` is the engine's `Machine::save_state` (both cores' architectural state,
//! SRAM, RTC RAM, PSRAM, flash, flash MMU, every peripheral, pending device time and DMA);
//! `BORD` the NOTE board (panel controller RAM, visible image and BUSY timing, rails, latch,
//! LED, charger, battery, inputs; the RTC and codec registers travel in `MACH` with the I2C
//! bus that owns them); `NOTE` the adapter's own counters and UART0 backlog. `HELD`, `BTNQ` and
//! `ENDP` carry host policy: keys held at save (released on restore), queued button edges and
//! host listeners to rebind. Code caches (decode, blocks, JIT) are never stored; restore
//! invalidates them. Host sockets are not machine state: restore closes guest flows, rebinds
//! the saved listeners and drops the Wi-Fi link so the guest reconnects.
//!
//! The section encodings (`emu_core::snap`) are not self-describing. `STATE_LAYOUT` names the
//! layout of the serialized types; bump it whenever a serialized engine or board type changes.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::str::FromStr;

use note_core::Profile;
use sha2::{Digest, Sha256};

pub const FORMAT_VERSION: u32 = 1;
pub const SCHEMA: u32 = 2;
/// Layout of the serialized machine/board types. Part of the engine identity.
pub const STATE_LAYOUT: u32 = 4;
pub const MAGIC: &[u8; 8] = b"N4SNAP1\0";
pub const ENGINE_ID: &str = "esp32sim";
/// Upstream esp32sim revision this fork started from, and the state layout on top of it.
pub const ENGINE_REVISION: &str = "4ab7e90+layout4";
pub const FLASH_CHECKPOINT_LABEL: &str = "Flash checkpoint";
/// SNAP-05 passed on NOTE4 (demo menu) and NOTE4C (diag-board across an RTC alarm) on
/// 2026-09-28: `note-emu --avd --quick-boot` resumes, and falls back to a cold boot when the
/// image is missing, rejected or paired with other flash.
pub const QUICK_BOOT_AVAILABLE: bool = true;

const FLAG_BOUNDARY: u32 = 1;
/// Machine, board and flash together exceed 24 MiB on a 16 MiB-flash, 8 MiB-PSRAM NOTE.
const MAX_SECTION: usize = 64 << 20;
const MAX_ENDPOINTS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapError {
    BadMagic,
    Truncated,
    BadChecksum,
    Trailing,
    Version(u32),
    Schema(u32),
    Engine(String),
    Profile(String),
    Firmware,
    Kind,
    NotAtBoundary,
    Unsupported(&'static str),
    DebugHooks,
    Range,
    Section(&'static str),
    Corrupt(&'static str),
    EndpointOccupied(String),
    Io(String),
    /// The machine or board state did not fit this machine (sizes, layout, device set).
    State(String),
}

impl std::fmt::Display for SnapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapError::BadMagic => write!(f, "bad snapshot magic"),
            SnapError::Truncated => write!(f, "truncated snapshot"),
            SnapError::BadChecksum => write!(f, "snapshot checksum mismatch"),
            SnapError::Trailing => write!(f, "trailing bytes after snapshot"),
            SnapError::Version(v) => write!(f, "snapshot version {v}"),
            SnapError::Schema(v) => write!(f, "snapshot schema {v}"),
            SnapError::Engine(id) => write!(f, "snapshot engine {id}"),
            SnapError::Profile(id) => write!(f, "snapshot profile {id}"),
            SnapError::Firmware => write!(f, "snapshot firmware mismatch"),
            SnapError::Kind => write!(f, "snapshot kind mismatch"),
            SnapError::NotAtBoundary => write!(f, "snapshot is not at a quantum boundary"),
            SnapError::Unsupported(dev) => write!(f, "unsupported device {dev} is active"),
            SnapError::DebugHooks => write!(f, "stubs or function probes are active"),
            SnapError::Range => write!(f, "snapshot range does not fit the machine"),
            SnapError::Section(name) => write!(f, "snapshot section {name}"),
            SnapError::Corrupt(what) => write!(f, "corrupt snapshot ({what})"),
            SnapError::EndpointOccupied(addr) => write!(f, "host endpoint {addr} is occupied"),
            SnapError::Io(err) => write!(f, "snapshot io: {err}"),
            SnapError::State(err) => write!(f, "snapshot state: {err}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Full = 1,
    FlashCheckpoint = 2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub engine_id: String,
    pub engine_revision: String,
    pub profile_id: String,
    pub profile_hash: [u8; 32],
    pub firmware_hash: [u8; 32],
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldKey {
    pub id: String,
    pub gpio: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedButton {
    pub gpio: u8,
    pub level: bool,
    pub at_cycle: u64,
}

/// A host listener to rebind after restore. The address is whatever was leased, never a constant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostLease {
    pub addr: SocketAddr,
    pub guest_port: u16,
    pub softap: bool,
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub kind: Kind,
    pub identity: Identity,
    pub flags: u32,
    pub cycles: u64,
    pub quantum: u64,
    /// `MACH`: engine machine state. Empty for a flash checkpoint.
    pub machine: Vec<u8>,
    /// `BORD`: NOTE board state.
    pub board: Vec<u8>,
    /// `NOTE`: adapter state.
    pub note: Vec<u8>,
    /// `FLSH`: flash checkpoint payload (persistent bytes). Empty for a machine snapshot.
    pub flash: Vec<u8>,
    pub held: Vec<HeldKey>,
    pub queue: Vec<QueuedButton>,
    pub endpoints: Vec<HostLease>,
}

pub struct Meta {
    pub identity: Identity,
    pub at_boundary: bool,
    pub held: Vec<HeldKey>,
    pub queue: Vec<QueuedButton>,
    pub endpoints: Vec<HostLease>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreReport {
    pub partial: bool,
    pub released: Vec<String>,
    pub epoch: u64,
    pub reconnect_pending: bool,
    pub occupied: Vec<String>,
    pub host_sockets: usize,
    pub full_frame: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuickBootImage {
    pub bytes: Vec<u8>,
    pub failures: u32,
    pub disabled: bool,
    pub last_error: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColdBoot {
    pub reason: String,
    pub flash_wiped: bool,
    pub quick_boot: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub label: &'static str,
    pub boot: &'static str,
    pub flash: Vec<u8>,
}

pub fn profile_hash(profile: &Profile) -> [u8; 32] {
    let mut hasher = Sha256::new();
    // The ID and revision alone do not cover changed pins or panel parameters.
    // Struct serialization has a stable field order within this format revision.
    hasher.update(serde_json::to_vec(profile).expect("profile serializes"));
    let dig = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&dig);
    out
}

pub fn identity_for(profile: &Profile, firmware_hash: [u8; 32]) -> Identity {
    Identity {
        engine_id: ENGINE_ID.into(),
        engine_revision: ENGINE_REVISION.into(),
        profile_id: profile.id.clone(),
        profile_hash: profile_hash(profile),
        firmware_hash,
    }
}


/// Build a machine snapshot from already-serialized sections.
pub fn capture(m: &esp32s3::Machine, board: Vec<u8>, note: Vec<u8>, meta: &Meta) -> Result<Image, SnapError> {
    if !meta.at_boundary {
        return Err(SnapError::NotAtBoundary);
    }
    if !m.stubs.is_empty() || !m.fn_probes.is_empty() {
        return Err(SnapError::DebugHooks);
    }
    if meta.endpoints.len() > MAX_ENDPOINTS {
        return Err(SnapError::Corrupt("endpoints"));
    }
    if meta.held.len() > 16 || meta.queue.len() > 32 {
        return Err(SnapError::Corrupt("input queue"));
    }
    let machine = m.save_state().map_err(|e| SnapError::State(e.to_string()))?;
    Ok(Image {
        kind: Kind::Full,
        identity: meta.identity.clone(),
        flags: FLAG_BOUNDARY,
        cycles: m.bus.cycles,
        quantum: m.quantum,
        machine,
        board,
        note,
        flash: Vec::new(),
        held: meta.held.clone(),
        queue: meta.queue.clone(),
        endpoints: meta.endpoints.clone(),
    })
}

pub fn encode(image: &Image) -> Vec<u8> {
    let mut w = W::default();
    w.bytes(MAGIC);
    w.u32(FORMAT_VERSION);
    w.u32(image.kind as u32);
    w.u32(SCHEMA);
    w.u32(image.flags);
    w.u64(image.cycles);
    w.u64(image.quantum);
    w.str(&image.identity.engine_id);
    w.str(&image.identity.engine_revision);
    w.str(&image.identity.profile_id);
    w.bytes(&image.identity.profile_hash);
    w.bytes(&image.identity.firmware_hash);
    let sections: Vec<(&[u8; 4], Vec<u8>)> = match image.kind {
        Kind::Full => vec![
            (b"MACH", image.machine.clone()),
            (b"BORD", image.board.clone()),
            (b"NOTE", image.note.clone()),
            (b"HELD", pack_held(&image.held)),
            (b"BTNQ", pack_queue(&image.queue)),
            (b"ENDP", pack_endpoints(&image.endpoints)),
        ],
        Kind::FlashCheckpoint => vec![(b"FLSH", image.flash.clone())],
    };
    w.u32(sections.len() as u32);
    for (tag, payload) in &sections {
        w.bytes(*tag);
        w.u32(payload.len() as u32);
        w.bytes(payload);
        w.u32(crc32(payload));
    }
    let sum = crc32(&w.0);
    w.u32(sum);
    w.0
}

pub fn encode_checkpoint(identity: &Identity, flash: &[u8]) -> Vec<u8> {
    encode(&Image {
        kind: Kind::FlashCheckpoint,
        identity: identity.clone(),
        flags: 0,
        cycles: 0,
        quantum: 0,
        machine: Vec::new(),
        board: Vec::new(),
        note: Vec::new(),
        flash: flash.to_vec(),
        held: Vec::new(),
        queue: Vec::new(),
        endpoints: Vec::new(),
    })
}

/// Validate the blob's framing, checksums and sections. Does not touch a machine.
pub fn decode(bytes: &[u8]) -> Result<Image, SnapError> {
    if bytes.len() < 12 {
        return Err(if bytes.len() >= 8 && &bytes[..8] != MAGIC { SnapError::BadMagic } else { SnapError::Truncated });
    }
    if &bytes[..8] != MAGIC {
        return Err(SnapError::BadMagic);
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    if version != FORMAT_VERSION {
        return Err(SnapError::Version(version));
    }
    let (body, trailer) = bytes.split_at(bytes.len() - 4);
    let expect = u32::from_le_bytes(trailer.try_into().unwrap());
    if crc32(body) != expect {
        return Err(SnapError::BadChecksum);
    }
    let mut r = R { b: body, i: 8 };
    let _version = r.u32()?;
    let kind = match r.u32()? {
        1 => Kind::Full,
        2 => Kind::FlashCheckpoint,
        _ => return Err(SnapError::Corrupt("kind")),
    };
    let schema = r.u32()?;
    if schema != SCHEMA {
        return Err(SnapError::Schema(schema));
    }
    let flags = r.u32()?;
    let cycles = r.u64()?;
    let quantum = r.u64()?;
    let identity = Identity {
        engine_id: r.str_()?,
        engine_revision: r.str_()?,
        profile_id: r.str_()?,
        profile_hash: r.arr32()?,
        firmware_hash: r.arr32()?,
    };
    let n = r.u32()? as usize;
    if n > 32 {
        return Err(SnapError::Corrupt("sections"));
    }
    let mut sections: Vec<([u8; 4], Vec<u8>)> = Vec::with_capacity(n);
    for _ in 0..n {
        let tag: [u8; 4] = r.take(4)?.try_into().unwrap();
        let len = r.u32()? as usize;
        if len > MAX_SECTION {
            return Err(SnapError::Corrupt("section length"));
        }
        let payload = r.take(len)?.to_vec();
        let sum = r.u32()?;
        if crc32(&payload) != sum {
            return Err(SnapError::BadChecksum);
        }
        if sections.iter().any(|(t, _)| *t == tag) {
            return Err(SnapError::Corrupt("duplicate section"));
        }
        sections.push((tag, payload));
    }
    if r.i != body.len() {
        return Err(SnapError::Trailing);
    }
    let allow: &[&[u8; 4]] = match kind {
        Kind::Full => &[b"MACH", b"BORD", b"NOTE", b"HELD", b"BTNQ", b"ENDP"],
        Kind::FlashCheckpoint => &[b"FLSH"],
    };
    for (tag, _) in &sections {
        if !allow.iter().any(|a| *a == tag) {
            return Err(SnapError::Corrupt("section"));
        }
    }
    let mut take = |tag: &[u8; 4]| -> Result<Vec<u8>, SnapError> {
        let i = sections.iter().position(|(t, _)| t == tag).ok_or(SnapError::Section(tag_name(tag)))?;
        Ok(std::mem::take(&mut sections[i].1))
    };
    let mut image = Image {
        kind,
        identity,
        flags,
        cycles,
        quantum,
        machine: Vec::new(),
        board: Vec::new(),
        note: Vec::new(),
        flash: Vec::new(),
        held: Vec::new(),
        queue: Vec::new(),
        endpoints: Vec::new(),
    };
    match kind {
        Kind::FlashCheckpoint => image.flash = take(b"FLSH")?,
        Kind::Full => {
            image.machine = take(b"MACH")?;
            image.board = take(b"BORD")?;
            image.note = take(b"NOTE")?;
            image.held = unpack_held(&take(b"HELD")?)?;
            image.queue = unpack_queue(&take(b"BTNQ")?)?;
            image.endpoints = unpack_endpoints(&take(b"ENDP")?)?;
        }
    }
    Ok(image)
}

pub fn check_identity(image: &Image, expect: &Identity) -> Result<(), SnapError> {
    if image.identity.engine_id != expect.engine_id || image.identity.engine_revision != expect.engine_revision {
        return Err(SnapError::Engine(image.identity.engine_id.clone()));
    }
    if image.identity.profile_id != expect.profile_id || image.identity.profile_hash != expect.profile_hash {
        return Err(SnapError::Profile(image.identity.profile_id.clone()));
    }
    if image.identity.firmware_hash != expect.firmware_hash {
        return Err(SnapError::Firmware);
    }
    Ok(())
}

/// Decode and check identity and form. No machine writes; state sections are validated when
/// applied (a mismatch there is reported before the machine is changed, see `NoteMachine`).
pub fn prepare(bytes: &[u8], expect: &Identity, m: &esp32s3::Machine) -> Result<Image, SnapError> {
    let image = decode(bytes)?;
    check_identity(&image, expect)?;
    if image.kind != Kind::Full {
        return Err(SnapError::Kind);
    }
    if image.flags != FLAG_BOUNDARY || image.quantum == 0 {
        return Err(SnapError::NotAtBoundary);
    }
    if !m.stubs.is_empty() || !m.fn_probes.is_empty() {
        return Err(SnapError::DebugHooks);
    }
    Ok(image)
}

pub fn read_flash_checkpoint(bytes: &[u8], expect: &Identity) -> Result<Checkpoint, SnapError> {
    let image = decode(bytes)?;
    check_identity(&image, expect)?;
    if image.kind != Kind::FlashCheckpoint {
        return Err(SnapError::Kind);
    }
    Ok(Checkpoint { label: FLASH_CHECKPOINT_LABEL, boot: "cold", flash: image.flash })
}

/// A failed or unavailable quick boot cold-boots. `persistent` is not written.
pub fn attempt_quick_boot(image: &mut QuickBootImage, persistent: &[u8]) -> ColdBoot {
    let _ = persistent;
    let reason = if image.disabled {
        "quick-boot image disabled after repeated failure".to_string()
    } else if image.bytes.is_empty() {
        "no quick-boot image".to_string()
    } else if !QUICK_BOOT_AVAILABLE {
        "quick boot is not available until SNAP-05 passes on NOTE4 and NOTE4C".to_string()
    } else {
        match decode(&image.bytes) {
            Ok(img) if img.kind != Kind::Full => "not a machine snapshot".to_string(),
            Ok(_) => String::new(),
            Err(e) => e.to_string(),
        }
    };
    if QUICK_BOOT_AVAILABLE && reason.is_empty() {
        return ColdBoot { reason: String::new(), flash_wiped: false, quick_boot: true };
    }
    image.failures = image.failures.saturating_add(1);
    image.last_error = reason.clone();
    if image.failures >= 2 {
        image.disabled = true;
    }
    ColdBoot { reason, flash_wiped: false, quick_boot: false }
}

pub fn take_due(queue: &mut Vec<QueuedButton>, cycle: u64) -> Vec<QueuedButton> {
    let mut due = Vec::new();
    queue.retain(|ev| {
        if ev.at_cycle <= cycle {
            due.push(ev.clone());
            false
        } else {
            true
        }
    });
    due
}

/// Rebind one leased host endpoint. Port 0 and the product's forbidden ports are rejected.
/// A different bound port is closed again rather than kept.
pub fn rebind_forward(net: &mut esp32s3::net::VirtualNet, addr: SocketAddr, mac: [u8; 6], guest_port: u16) -> Result<SocketAddr, SnapError> {
    if forbidden_port(addr.port()) {
        return Err(SnapError::EndpointOccupied(addr.to_string()));
    }
    match net.listen_forward(addr, mac, guest_port) {
        Ok(bound) if bound == addr => Ok(bound),
        Ok(bound) => {
            let _ = net.close_forward(bound);
            Err(SnapError::EndpointOccupied(addr.to_string()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse || e.kind() == std::io::ErrorKind::AddrNotAvailable => {
            Err(SnapError::EndpointOccupied(addr.to_string()))
        }
        Err(e) => Err(SnapError::Io(e.to_string())),
    }
}

pub(crate) fn forbidden_port(port: u16) -> bool {
    port == 0 || port == 8080 || (8090..8100).contains(&port)
}

/// Write `bytes` to a temporary sibling, then rename over `path`.
pub fn publish(path: &Path, bytes: &[u8]) -> Result<(), SnapError> {
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp_name);
    std::fs::write(&tmp, bytes).map_err(|e| SnapError::Io(e.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        SnapError::Io(e.to_string())
    })
}

pub fn host_socket_count(m: &esp32s3::Machine) -> usize {
    let wifi = &m.bus.periph.wifi;
    let station = wifi.net.as_ref().and_then(|n| n.nat.as_ref()).map(|n| n.host_socket_count()).unwrap_or(0);
    let softap = wifi.relay.as_ref().map(|r| r.host_socket_count()).unwrap_or(0);
    station + softap
}

fn tag_name(tag: &[u8; 4]) -> &'static str {
    match tag {
        b"MACH" => "MACH",
        b"BORD" => "BORD",
        b"NOTE" => "NOTE",
        b"FLSH" => "FLSH",
        b"HELD" => "HELD",
        b"BTNQ" => "BTNQ",
        b"ENDP" => "ENDP",
        _ => "unknown",
    }
}

fn pack_held(keys: &[HeldKey]) -> Vec<u8> {
    let mut w = W::default();
    w.u16(keys.len() as u16);
    for key in keys {
        w.u8(key.gpio);
        w.str(&key.id);
    }
    w.0
}

fn unpack_held(p: &[u8]) -> Result<Vec<HeldKey>, SnapError> {
    let mut r = R { b: p, i: 0 };
    let n = r.u16()? as usize;
    if n > 16 {
        return Err(SnapError::Corrupt("held"));
    }
    let mut keys = Vec::with_capacity(n);
    for _ in 0..n {
        keys.push(HeldKey { gpio: r.u8()?, id: r.str_()? });
    }
    if r.i != p.len() {
        return Err(SnapError::Corrupt("held"));
    }
    Ok(keys)
}

fn pack_queue(queue: &[QueuedButton]) -> Vec<u8> {
    let mut w = W::default();
    w.u16(queue.len() as u16);
    for ev in queue {
        w.u8(ev.gpio);
        w.u8(u8::from(ev.level));
        w.u64(ev.at_cycle);
    }
    w.0
}

fn unpack_queue(p: &[u8]) -> Result<Vec<QueuedButton>, SnapError> {
    let mut r = R { b: p, i: 0 };
    let n = r.u16()? as usize;
    if n > 32 {
        return Err(SnapError::Corrupt("queue"));
    }
    let mut queue = Vec::with_capacity(n);
    for _ in 0..n {
        queue.push(QueuedButton { gpio: r.u8()?, level: r.u8()? != 0, at_cycle: r.u64()? });
    }
    if r.i != p.len() {
        return Err(SnapError::Corrupt("queue"));
    }
    Ok(queue)
}

fn pack_endpoints(endpoints: &[HostLease]) -> Vec<u8> {
    let mut w = W::default();
    w.u16(endpoints.len() as u16);
    for ep in endpoints {
        w.u16(ep.addr.port());
        w.u8(u8::from(ep.softap));
        w.u16(ep.guest_port);
        w.str(&ep.addr.ip().to_string());
    }
    w.0
}

fn unpack_endpoints(p: &[u8]) -> Result<Vec<HostLease>, SnapError> {
    let mut r = R { b: p, i: 0 };
    let n = r.u16()? as usize;
    if n > MAX_ENDPOINTS {
        return Err(SnapError::Corrupt("endpoints"));
    }
    let mut endpoints = Vec::with_capacity(n);
    for _ in 0..n {
        let port = r.u16()?;
        let softap = r.u8()? != 0;
        let guest_port = r.u16()?;
        let ip = IpAddr::from_str(&r.str_()?).map_err(|_| SnapError::Corrupt("endpoint"))?;
        endpoints.push(HostLease { addr: SocketAddr::new(ip, port), guest_port, softap });
    }
    if r.i != p.len() {
        return Err(SnapError::Corrupt("endpoints"));
    }
    Ok(endpoints)
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, v: &[u8]) {
        self.0.extend_from_slice(v);
    }
    fn str(&mut self, v: &str) {
        let b = v.as_bytes();
        self.u16(b.len() as u16);
        self.bytes(b);
    }
}

struct R<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SnapError> {
        if self.b.len() - self.i < n {
            return Err(SnapError::Truncated);
        }
        let s = &self.b[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, SnapError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, SnapError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, SnapError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes(b.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, SnapError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes(b.try_into().unwrap()))
    }
    fn arr32(&mut self) -> Result<[u8; 32], SnapError> {
        Ok(self.take(32)?.try_into().unwrap())
    }
    fn str_(&mut self) -> Result<String, SnapError> {
        let n = self.u16()? as usize;
        if n > 64 {
            return Err(SnapError::Corrupt("string"));
        }
        let b = self.take(n)?;
        String::from_utf8(b.to_vec()).map_err(|_| SnapError::Corrupt("utf8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::NoteMachine;
    use crate::replay::Guest;
    use crate::ssd2683::{Ssd2683, Timing};
    use emu_core::Core;
    use note_core::profile;
    use note_core::PanelVariant;
    use std::net::{Ipv4Addr, TcpListener};

    /// Arbitrary probe locations (schema 2 stores all memory; these are just places to look).
    const PROBE_SRAM_OFF: u32 = 0x120;
    const PROBE_FLASH_OFF: u32 = 0x20;
    const FW: [u8; 32] = [0x11; 32];
    const OTHER_FW: [u8; 32] = [0x22; 32];

    fn profile(id: &str) -> Profile {
        profile::find(&profile::profiles_dir(), id).unwrap()
    }

    fn sram_word(m: &esp32s3::Machine) -> u32 {
        let i = PROBE_SRAM_OFF as usize;
        u32::from_le_bytes(m.bus.sram[i..i + 4].try_into().unwrap())
    }

    fn flash_byte(m: &esp32s3::Machine) -> u8 {
        m.bus.flash[PROBE_FLASH_OFF as usize]
    }

    fn reseal(buf: &mut Vec<u8>) {
        let n = buf.len();
        let sum = crc32(&buf[..n - 4]);
        buf[n - 4..].copy_from_slice(&sum.to_le_bytes());
    }

    #[test]
    fn crc32_matches_the_check_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn identity_covers_profile_hardware_fields() {
        let base = profile("note4c");
        let mut changed = base.clone();
        changed.buttons[0].gpio ^= 1;
        assert_ne!(profile_hash(&base), profile_hash(&changed));
        let mut changed = base.clone();
        changed.display.width += 1;
        assert_ne!(profile_hash(&base), profile_hash(&changed));
    }

    #[test]
    fn snap01_round_trip_on_note4_and_note4c() {
        for id in ["note4", "note4c"] {
            let mut machine = NoteMachine::bare(&profile(id));
            let pc0 = 0x4200_0AB0;
            let pc1 = 0x4000_0C01;
            let word = 0xA1B2_C3D4u32;
            let byte = 0x5A;
            machine.m.cores[0].pc = pc0;
            machine.m.cores[1].pc = pc1;
            machine.m.set_core_held(1, true);
            machine.m.cores[0].ccount = 1000;
            machine.m.cores[0].ccompare[0] = 5000;
            let off = PROBE_SRAM_OFF as usize;
            machine.m.bus.sram[off..off + 4].copy_from_slice(&word.to_le_bytes());
            machine.m.bus.flash[PROBE_FLASH_OFF as usize] = byte;
            let mut frame = machine.board.lock().panel.visible().to_vec();
            frame[0] ^= 0x0f;
            machine.board.lock().panel.restore_visible(&frame).unwrap();

            let blob = machine.save_snapshot(FW).unwrap();
            assert!(blob.len() > machine.m.bus.flash.len() + machine.m.bus.sram.len(), "schema 2 stores the whole machine, flash included");
            assert!(blob.windows(8).all(|w| w != b"JITCODE!"));

            machine.m.cores[0].pc = 0x1111_1111;
            machine.m.cores[1].pc = 0x2222_2222;
            machine.m.set_core_held(1, false);
            machine.m.bus.sram[off..off + 4].copy_from_slice(&0x2222_2222u32.to_le_bytes());
            machine.m.bus.flash[PROBE_FLASH_OFF as usize] = 0x33;
            let mut dirty = frame.clone();
            dirty[0] ^= 0xff;
            machine.board.lock().panel.restore_visible(&dirty).unwrap();
            let flushes = machine.m.cores[0].code_cache_stats().unwrap().1;

            let report = machine.restore_snapshot(&blob, FW).unwrap();
            assert!(!report.partial, "{id} schema 2 is the whole machine");
            assert!(report.full_frame);
            assert_eq!(machine.m.cores[0].pc, pc0, "{id}");
            assert_eq!(machine.m.cores[1].pc, pc1, "{id}");
            assert!(machine.m.core_is_held(1), "{id}");
            assert_eq!(sram_word(&machine.m), word, "{id}");
            assert_eq!(flash_byte(&machine.m), byte, "{id}");
            assert_eq!(machine.frame(), frame, "{id}");
            assert!(machine.m.cores[0].code_cache_stats().unwrap().1 > flushes, "{id} block cache flushed");
            machine.button("ok", true).unwrap();
            machine.button("ok", false).unwrap();
            assert_eq!(machine.frame(), frame, "{id} button does not replace the restored frame");
        }
    }

    #[test]
    fn snap03_rejects_bad_blobs_before_replacing_state() {
        let mut machine = NoteMachine::bare(&profile("note4c"));
        machine.m.cores[0].pc = 0x4200_0AB0;
        let off = PROBE_SRAM_OFF as usize;
        machine.m.bus.sram[off..off + 4].copy_from_slice(&0xA1B2_C3D4u32.to_le_bytes());
        machine.m.bus.flash[PROBE_FLASH_OFF as usize] = 0x5A;
        let blob = machine.save_snapshot(FW).unwrap();

        machine.m.cores[0].pc = 0x1111_1111;
        machine.m.bus.sram[off..off + 4].copy_from_slice(&0x2222_2222u32.to_le_bytes());
        machine.m.bus.flash[PROBE_FLASH_OFF as usize] = 0x33;

        let mut bad_magic = blob.clone();
        bad_magic[0] = b'X';
        let mut truncated = blob.clone();
        truncated.pop();
        let mut bad_sum = blob.clone();
        let mid = bad_sum.len() / 2;
        bad_sum[mid] ^= 0xff;

        for (blob, kind) in [(&bad_magic, "magic"), (&truncated, "truncated"), (&bad_sum, "checksum")] {
            let err = machine.restore_snapshot(blob, FW).unwrap_err();
            assert!(matches!(err, SnapError::BadMagic | SnapError::Truncated | SnapError::BadChecksum), "{kind}: {err}");
            assert_eq!(machine.m.cores[0].pc, 0x1111_1111, "{kind}");
            assert_eq!(sram_word(&machine.m), 0x2222_2222, "{kind}");
            assert_eq!(flash_byte(&machine.m), 0x33, "{kind}");
        }

        let mut version = blob.clone();
        version[8..12].copy_from_slice(&99u32.to_le_bytes());
        assert!(matches!(machine.restore_snapshot(&version, FW).unwrap_err(), SnapError::Version(99)));
        assert_eq!(flash_byte(&machine.m), 0x33);

        let mut note4 = NoteMachine::bare(&profile("note4"));
        note4.m.cores[0].pc = 0x4200_0100;
        let foreign = note4.save_snapshot(FW).unwrap();
        assert!(matches!(machine.restore_snapshot(&foreign, FW).unwrap_err(), SnapError::Profile(_)));
        assert_eq!(machine.m.cores[0].pc, 0x1111_1111);

        assert!(matches!(machine.restore_snapshot(&blob, OTHER_FW).unwrap_err(), SnapError::Firmware));
        let mut section = blob.clone();
        let flip = section.len() - 8;
        section[flip] ^= 0xff;
        reseal(&mut section);
        assert!(matches!(machine.restore_snapshot(&section, FW).unwrap_err(), SnapError::BadChecksum));

        let image = decode(&blob).unwrap();
        let mut wrong = image.clone();
        wrong.identity.engine_id = "other".into();
        let wrong_blob = encode(&wrong);
        assert!(matches!(machine.restore_snapshot(&wrong_blob, FW).unwrap_err(), SnapError::Engine(_)));
        let mut wrong_rev = image.clone();
        wrong_rev.identity.engine_revision = "0000000".into();
        assert!(matches!(machine.restore_snapshot(&encode(&wrong_rev), FW).unwrap_err(), SnapError::Engine(_)));
        // A machine section that does not fit is detected while applying: the machine is put back.
        let mut short = image.clone();
        short.machine.truncate(short.machine.len() - 1);
        assert!(matches!(machine.restore_snapshot(&encode(&short), FW).unwrap_err(), SnapError::State(_)));
        let mut late = image.clone();
        let n = late.board.len();
        late.board.truncate(n - 1);
        assert!(matches!(machine.restore_snapshot(&encode(&late), FW).unwrap_err(), SnapError::State(_)), "board fails after the machine applied");
        let mut forged = image;
        forged.flags = FLAG_BOUNDARY | 2;
        assert!(matches!(machine.restore_snapshot(&encode(&forged), FW).unwrap_err(), SnapError::NotAtBoundary));
        assert_eq!(machine.m.cores[0].pc, 0x1111_1111);
        assert_eq!(sram_word(&machine.m), 0x2222_2222);
        assert_eq!(flash_byte(&machine.m), 0x33, "flash and memory stay together");

        machine.restore_snapshot(&blob, FW).unwrap();
        assert_eq!(machine.m.cores[0].pc, 0x4200_0AB0);
        assert_eq!(sram_word(&machine.m), 0xA1B2_C3D4);
        assert_eq!(flash_byte(&machine.m), 0x5A);
    }

    #[test]
    fn save_refuses_a_slice_and_debug_hooks_and_keeps_every_device() {
        let mut machine = NoteMachine::bare(&profile("note4"));
        machine.in_slice = true;
        assert!(matches!(machine.save_snapshot(FW).unwrap_err(), SnapError::NotAtBoundary));
        machine.in_slice = false;
        machine.m.stubs.insert(0x4000_0000, 1);
        assert!(matches!(machine.save_snapshot(FW).unwrap_err(), SnapError::DebugHooks));
        machine.m.stubs.clear();
        machine.m.fn_probes.insert(0x4000_0000, "probe".into());
        assert!(matches!(machine.save_snapshot(FW).unwrap_err(), SnapError::DebugHooks));
        machine.m.fn_probes.clear();
        // Schema 2 carries RMT, PCNT and LCD_CAM like every other peripheral.
        machine.m.bus.periph.rmt.tx_count = 1;
        machine.m.bus.periph.pcnt.cnt[0] = 3;
        let blob = machine.save_snapshot(FW).unwrap();
        machine.m.bus.periph.rmt.tx_count = 0;
        machine.m.bus.periph.pcnt.cnt[0] = 0;
        machine.restore_snapshot(&blob, FW).unwrap();
        assert_eq!((machine.m.bus.periph.rmt.tx_count, machine.m.bus.periph.pcnt.cnt[0]), (1, 3));
    }

    #[test]
    fn snap02_busy_queue_timer_and_dma_continue() {
        let mut machine = NoteMachine::bare(&profile("note4c"));
        {
            let mut st = machine.board.lock();
            st.panel.set_power(true);
            st.panel.set_reset(false);
            st.panel.set_reset(true);
            assert!(!st.panel.busy_level());
        }
        let deadline = machine.board.lock().panel.next_deadline().unwrap();
        machine.m.bus.periph.gpio.set_input(21, false);
        machine.board.lock().inputs.push((21, false));
        machine.queue_button(21, true, deadline);
        let s = &mut machine.m.bus.periph.systimer;
        s.conf = (1 << 30) | (1 << 24);
        s.unit[0] = 100;
        s.target[0] = 150;
        s.armed[0] = true;
        s.int_ena = 1;
        machine.m.cores[0].ccount = 1000;
        machine.m.cores[0].ccompare[0] = 5000;
        machine.m.cores[0].intenable = 0x20;
        let d = &mut machine.m.bus.periph.gdma.out[0];
        d.desc = 0x3FC0_1234;
        d.buf_pos = 16;
        d.running = true;
        d.int_raw = 1;
        d.int_ena = 1;

        let blob = machine.save_snapshot(FW).unwrap();

        machine.board.lock().panel.advance_to(deadline);
        assert!(machine.board.lock().panel.busy_level());
        machine.button_queue.clear();
        machine.m.bus.periph.systimer.unit[0] = 0;
        machine.m.bus.periph.systimer.armed[0] = false;
        machine.m.cores[0].ccount = 0;
        machine.m.cores[0].ccompare[0] = 0;
        machine.m.cores[0].intenable = 0;
        machine.m.bus.periph.gdma.out[0].desc = 0;
        machine.m.bus.periph.gdma.out[0].buf_pos = 0;
        machine.m.bus.periph.gdma.out[0].running = false;

        machine.restore_snapshot(&blob, FW).unwrap();
        assert!(!machine.board.lock().panel.busy_level());
        assert_eq!(machine.board.lock().panel.next_deadline(), Some(deadline));
        machine.board.lock().panel.advance_to(deadline);
        assert!(machine.board.lock().panel.busy_level(), "BUSY releases at the restored deadline");

        assert_eq!(machine.button_queue.len(), 1);
        let still_down = machine.board.lock().inputs.iter().find(|(pin, _)| *pin == 21).unwrap().1;
        assert!(!still_down, "the release stays queued until its cycle");
        machine.m.bus.cycles = deadline - 1;
        machine.apply_queued_buttons();
        assert_eq!(machine.button_queue.len(), 1);
        machine.m.bus.cycles = deadline;
        machine.apply_queued_buttons();
        assert!(machine.button_queue.is_empty());
        let level = machine.board.lock().inputs.iter().find(|(pin, _)| *pin == 21).unwrap().1;
        assert!(level, "queued release level is applied at its cycle");

        let s = &mut machine.m.bus.periph.systimer;
        s.tick(50);
        assert!(s.irq(0), "systimer deadline still fires");
        assert_eq!(machine.m.cores[0].ccompare[0].wrapping_sub(machine.m.cores[0].ccount), 4000);
        assert_eq!(machine.m.cores[0].intenable, 0x20);
        assert_eq!(machine.m.bus.periph.gdma.read(0x90), 0x3FC0_1234);
        assert!(machine.m.bus.periph.gdma.out[0].irq());
    }

    #[test]
    fn snap04_closes_host_flows_releases_keys_and_reports_an_occupied_endpoint() {
        let mut machine = NoteMachine::bare(&profile("note4"));
        machine.button("down", true).unwrap();
        let guest_port = 4242u16;
        let addr = bind_ephemeral(&mut machine, guest_port);
        let blob = machine.save_snapshot(FW).unwrap();
        assert_eq!(host_socket_count(&machine.m), 1);

        let report = machine.restore_snapshot(&blob, FW).unwrap();
        assert!(report.released.iter().any(|id| id == "down"));
        assert!(!machine.board.lock().inputs.iter().any(|(pin, level)| *pin == 18 && !*level));
        // No virtual AP on a bare machine, so there is no station link to drop.
        assert_eq!(report.reconnect_pending, machine.m.bus.periph.wifi.ap.is_some());
        assert!(report.occupied.is_empty());
        assert_eq!(report.host_sockets, 1, "the leased endpoint was rebound");
        assert_eq!(report.epoch, 2);
        machine.close_saved_endpoints();

        let hold = TcpListener::bind(addr).unwrap();
        let blocked = machine.restore_snapshot(&blob, FW).unwrap();
        assert_eq!(blocked.occupied, vec![addr.to_string()]);
        assert_eq!(blocked.host_sockets, 0, "an occupied endpoint is not replaced with another port");
        assert!(TcpListener::bind(addr).is_err(), "the occupier still owns the endpoint");
        drop(hold);

        let mut relay = esp32s3::softap::SoftApRelay::new(machine.mac());
        let relay_addr = loop {
            let bound = relay.listen(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), guest_port).unwrap();
            if super::forbidden_port(bound.port()) {
                let _ = relay.on_snapshot_restore();
                continue;
            }
            break bound;
        };
        let _ = relay.on_snapshot_restore();
        TcpListener::bind(relay_addr).unwrap();
    }

    #[test]
    fn helper_owned_listener_survives_restore_without_rebinding() {
        for softap in [false, true] {
            let mut machine = NoteMachine::bare(&profile("note4c"));
            let helper = loop {
                let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
                if !forbidden_port(listener.local_addr().unwrap().port()) { break listener; }
            };
            let addr = helper.local_addr().unwrap();
            if softap {
                machine.adopt_softap(helper.try_clone().unwrap(), 80).unwrap();
            } else {
                machine.adopt_forward(helper.try_clone().unwrap(), 80).unwrap();
            }
            let blob = machine.save_snapshot(FW).unwrap();
            for _ in 0..2 {
                let report = machine.restore_snapshot(&blob, FW).unwrap();
                assert!(report.occupied.is_empty(), "helper still owns the address: reuse its listener");
                assert_eq!(report.host_sockets, 1);
            }
            machine.close_saved_endpoints();
            drop(helper);
            assert!(TcpListener::bind(addr).is_ok(), "explicit close releases retained listeners");
        }
    }

    #[test]
    fn softap_listener_is_counted_and_rebound() {
        let mut machine = NoteMachine::bare(&profile("note4c"));
        let addr = loop {
            let addr = machine.listen_softap(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), 8081).unwrap();
            if !forbidden_port(addr.port()) {
                break addr;
            }
            machine.close_saved_endpoints();
        };
        assert_eq!(host_socket_count(&machine.m), 1);
        let blob = machine.save_snapshot(FW).unwrap();
        let report = machine.restore_snapshot(&blob, FW).unwrap();
        assert!(report.occupied.is_empty());
        assert_eq!(report.host_sockets, 1);
        machine.close_saved_endpoints();
        assert!(TcpListener::bind(addr).is_ok());
    }

    #[test]
    fn snap05_failed_quick_boot_keeps_flash_and_a_checkpoint_cold_boots() {
        let persistent = vec![0x11, 0x22, 0x33, 0x44];
        let mut image = QuickBootImage { bytes: vec![0xff, 0x00], ..QuickBootImage::default() };
        let first = attempt_quick_boot(&mut image, &persistent);
        assert!(!first.quick_boot);
        assert!(!first.flash_wiped);
        assert_eq!(persistent, vec![0x11, 0x22, 0x33, 0x44]);
        assert_eq!(image.failures, 1);
        assert!(!image.disabled);
        let second = attempt_quick_boot(&mut image, &persistent);
        assert!(image.disabled);
        assert!(second.reason.contains("disabled") || image.failures >= 2);
        assert_eq!(persistent, vec![0x11, 0x22, 0x33, 0x44]);
        let third = attempt_quick_boot(&mut image, &persistent);
        assert!(!third.quick_boot && !third.flash_wiped);
        assert_eq!(persistent, [0x11, 0x22, 0x33, 0x44]);

        let mut machine = NoteMachine::bare(&profile("note4c"));
        let good = machine.save_snapshot(FW).unwrap();
        machine.store_quick_boot_image(good);
        let boot = machine.attempt_quick_boot(&persistent);
        assert!(QUICK_BOOT_AVAILABLE);
        assert!(boot.quick_boot, "a decodable machine snapshot quick-boots: {}", boot.reason);
        assert_eq!(persistent, vec![0x11, 0x22, 0x33, 0x44]);
        assert!(!boot.flash_wiped);

        machine.m.cores[0].pc = 0x4200_0AB0;
        let checkpoint = machine.save_flash_checkpoint(FW, &persistent).unwrap();
        machine.m.cores[0].pc = 0x9999;
        let mut stored = vec![0x01];
        let loaded = machine.load_flash_checkpoint(&checkpoint, FW, &mut stored).unwrap();
        assert_eq!(loaded.label, "Flash checkpoint");
        assert_eq!(loaded.boot, "cold");
        assert_eq!(stored, persistent);
        assert_eq!(machine.m.cores[0].pc, 0x9999, "a flash checkpoint does not replace CPU state");

        let full = machine.save_snapshot(FW).unwrap();
        let mut untouched = vec![0xAB];
        assert!(matches!(machine.load_flash_checkpoint(&full, FW, &mut untouched).unwrap_err(), SnapError::Kind));
        assert_eq!(untouched, vec![0xAB]);
        assert_eq!(machine.m.cores[0].pc, 0x9999);

        let mut wrong = decode(&checkpoint).unwrap();
        wrong.identity.firmware_hash = OTHER_FW;
        assert!(machine.load_flash_checkpoint(&encode(&wrong), FW, &mut untouched).is_err());
        assert_eq!(untouched, vec![0xAB]);

        let path = std::env::temp_dir().join(format!("n4snap-{}-{}.bin", std::process::id(), checkpoint.len()));
        std::fs::write(&path, b"old").unwrap();
        publish(&path, &checkpoint).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), checkpoint);
        assert!(!path.with_extension("bin.tmp").exists());
        let _ = std::fs::remove_file(&path);
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        assert!(!std::path::Path::new(&tmp).exists());
    }

    #[test]
    fn panel_anchor_round_trips_without_a_waveform_change() {
        let mut panel = Ssd2683::new(PanelVariant::Bwry, Timing::fast());
        panel.set_power(true);
        panel.set_reset(false);
        panel.set_reset(true);
        let anchor = panel.busy_anchor();
        let deadline = panel.next_deadline().unwrap();
        panel.advance_to(deadline);
        panel.restore_busy_anchor(anchor);
        assert_eq!(panel.next_deadline(), Some(deadline));
    }

    fn bind_ephemeral(machine: &mut NoteMachine, guest_port: u16) -> SocketAddr {
        loop {
            let addr = machine.listen_forward(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), guest_port).unwrap();
            if super::forbidden_port(addr.port()) {
                machine.close_forward(addr);
                continue;
            }
            return addr;
        }
    }
}
