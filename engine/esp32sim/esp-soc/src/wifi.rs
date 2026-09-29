//! The virtual air interface: 802.11 frame helpers and a minimal access point the emulated MAC
//! "hears". The AP beacons, answers probe requests, and completes open-system authentication and
//! association; data frames are handed to the network backend (docs/networking-plan.md).
//! [`VirtualAp::disconnect`] deauthenticates the station so the next authentication and
//! association are a new join. A disassociation leaves the station authenticated.

#[cfg(test)]
mod tests;

pub fn mac_str(m: &[u8]) -> String { m.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(":") }

fn ies(f: &[u8], body: usize) -> Vec<(u8, &[u8])> {
    let mut v = Vec::new(); let mut i = body;
    while i + 2 <= f.len() { let (id, l) = (f[i], f[i + 1] as usize); if i + 2 + l > f.len() { break; } v.push((id, &f[i + 2..i + 2 + l])); i += 2 + l; }
    v
}
fn mgmt_body_offset(subtype: u16) -> usize { match subtype { 8 | 5 => 24 + 12, 0 => 24 + 4, 1 => 24 + 6, 11 => 24 + 6, _ => 24 } }

/// One-line description of an 802.11 frame (frame control, addresses, SSID for management frames).
pub fn describe(f: &[u8]) -> String {
    if f.len() < 24 { return format!("{} bytes (short): {:02x?}", f.len(), f); }
    let fc = u16::from_le_bytes([f[0], f[1]]);
    let (ty, st) = ((fc >> 2) & 3, (fc >> 4) & 0xf);
    let kind = match (ty, st) {
        (0, 0) => "assoc-req", (0, 1) => "assoc-resp", (0, 4) => "probe-req", (0, 5) => "probe-resp", (0, 8) => "beacon",
        (0, 10) => "disassoc", (0, 11) => "auth", (0, 12) => "deauth", (0, 13) => "action",
        (1, 11) => "rts", (1, 12) => "cts", (1, 13) => "ack", (1, 10) => "ps-poll",
        (2, 0) => "data", (2, 4) => "null", (2, 8) => "qos-data", (2, 12) => "qos-null", _ => "?",
    };
    let mut s = format!("{} bytes {} ({}/{}) a1={} a2={} a3={}", f.len(), kind, ty, st, mac_str(&f[4..10]), mac_str(&f[10..16]), mac_str(&f[16..22]));
    if ty == 0 { for (id, d) in ies(f, mgmt_body_offset(st)) { if id == 0 { s += &format!(" ssid='{}'", String::from_utf8_lossy(d)); } } }
    if ty == 2 && f.len() >= 32 { let off = if st & 8 != 0 { 26 } else { 24 }; if f.len() >= off + 8 && f[off] == 0xaa { let et = u16::from_be_bytes([f[off + 6], f[off + 7]]); s += &format!(" ethertype={:#06x}", et); } }
    s
}

/// True for a beacon frame (management subtype 8).
pub fn is_beacon(f: &[u8]) -> bool { f.len() >= 2 && f[0] & 0x0c == 0 && (f[0] >> 4) & 0xf == 8 }

/// RSN information element advertised in beacons and echoed in handshake message 3: WPA2-PSK, CCMP.
pub const RSN_IE: &[u8] = &[48, 20, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 2, 0, 0];

#[derive(Clone, Debug)]
pub struct ApConfig { pub ssid: String, pub bssid: [u8; 6], pub channel: u8, pub psk: Option<String> }

impl ApConfig {
    /// Parse the shared CLI/browser AP configuration. Unknown keys are errors so a
    /// misspelled passphrase option cannot silently configure an open network.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut cfg = Self {
            ssid: "esp32sim".into(), bssid: [0x02, 0x53, 0x49, 0x4d, 0x00, 0x01], channel: 6, psk: None,
        };
        if spec.is_empty() { return Ok(cfg); }
        for entry in spec.split(',') {
            let (key, value) = entry.split_once('=')
                .ok_or_else(|| format!("invalid WiFi option '{entry}': expected key=value"))?;
            match key {
                "ssid" => cfg.ssid = value.to_string(),
                "chan" | "channel" | "ch" => {
                    cfg.channel = value.parse::<u8>().ok().filter(|n| (1..=14).contains(n))
                        .ok_or_else(|| format!("invalid WiFi channel '{value}': expected 1 through 14"))?;
                }
                "psk" | "password" | "pass" => cfg.psk = Some(value.to_string()),
                "bssid" => {
                    let invalid = || format!("invalid WiFi BSSID '{value}': expected six hexadecimal octets");
                    let mut octets = value.split(':');
                    for byte in &mut cfg.bssid {
                        let octet = octets.next().filter(|s| s.len() == 2 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                            .ok_or_else(&invalid)?;
                        *byte = u8::from_str_radix(octet, 16).map_err(|_| invalid())?;
                    }
                    if octets.next().is_some() { return Err(invalid()); }
                }
                _ => return Err(format!("unknown WiFi option '{key}'")),
            }
        }
        Ok(cfg)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StaState { Idle, Authenticated, Associated }

/// A frame the AP puts on the air, with the emulated time (µs) it should reach the station.
pub struct AirFrame { pub at_us: u64, pub frame: Vec<u8> }

/// WPA2 four-way handshake state (AP side).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WpaState {
    #[default]
    Idle,
    AwaitingMessage2,
    AwaitingMessage4,
    Installed,
}

#[derive(Default)]
pub struct Wpa {
    pub pmk: [u8; 32],
    pub anonce: [u8; 32],
    pub gtk: [u8; 16],
    pub replay: u64,
    pub state: WpaState,
}

pub struct VirtualAp {
    pub cfg: ApConfig,
    pub wpa: Wpa,
    pub state: StaState,
    pub sta: [u8; 6],
    pub aid: u16,
    pub next_beacon_us: u64,
    pub beacon_interval_us: u64,
    pub seq: u16,
    pub queue: Vec<AirFrame>,
    pub log: bool,
    pub stats: (u64, u64, u64),   // beacons, probe responses, data frames from the station
    pub pn: u64,                  // CCMP packet number for frames we send
    /// Association and reassociation responses sent. A reconnect increments this again.
    pub associations: u64,
    /// When the AP last deauthenticated a station that sent data without an association.
    pub class3_deauth_us: Option<u64>,
}

impl VirtualAp {
    pub fn new(cfg: ApConfig, log: bool) -> Self {
        let mut wpa = Wpa::default();
        if let Some(psk) = &cfg.psk {
            wpa.pmk.copy_from_slice(&esp_periph::crypto::pbkdf2_sha1(psk.as_bytes(), cfg.ssid.as_bytes(), 4096, 32));
            // deterministic nonces/GTK: the emulator must replay identically run to run
            let seed = esp_periph::crypto::sha1(&[&wpa.pmk[..], &cfg.bssid[..]].concat());
            for i in 0..32 { wpa.anonce[i] = seed[i % 20] ^ (i as u8); }
            for i in 0..16 { wpa.gtk[i] = seed[(i + 3) % 20] ^ 0x5a; }
        }
        VirtualAp { cfg, wpa, state: StaState::Idle, sta: [0; 6], aid: 1, next_beacon_us: 100_000, beacon_interval_us: 102_400, seq: 0,
                    queue: Vec::new(), pn: 0, log, stats: (0, 0, 0), associations: 0, class3_deauth_us: None }
    }
    /// Drop pairwise state. The PMK and the replay counter stay: the next handshake must
    /// use a higher counter, and the passphrase does not change.
    fn forget_keys(&mut self) {
        self.wpa.state = WpaState::Idle;
        self.pn = 0;
    }
    /// Deauthenticate the station that has joined. The queued frame is what its driver
    /// has to act on; clearing the air queue drops a handshake or DHCP reply from the
    /// old association. Returns false before any station has authenticated.
    pub fn disconnect(&mut self, now_us: u64) -> bool {
        if self.sta == [0; 6] { return false; }
        let sta = self.sta;
        self.state = StaState::Idle;
        self.forget_keys();
        self.queue.clear();
        let bssid = self.cfg.bssid;
        let mut frame = self.hdr(12 << 4, &sta, &bssid);
        frame.extend_from_slice(&2u16.to_le_bytes()); // previous authentication no longer valid
        self.send(now_us, frame);
        true
    }
    fn hdr(&mut self, fc: u16, a1: &[u8; 6], a3: &[u8; 6]) -> Vec<u8> {
        let mut f = Vec::with_capacity(128);
        f.extend_from_slice(&fc.to_le_bytes()); f.extend_from_slice(&[0, 0]);   // fc, duration
        f.extend_from_slice(a1); f.extend_from_slice(&self.cfg.bssid); f.extend_from_slice(a3);
        f.extend_from_slice(&(self.seq << 4).to_le_bytes()); self.seq = self.seq.wrapping_add(1);
        f
    }
    fn capability(&self) -> u16 { 0x0001 | if self.cfg.psk.is_some() { 0x0010 } else { 0 } | 0x0400 }   // ESS, privacy, short slot
    fn common_ies(&self, f: &mut Vec<u8>) {
        f.push(0); f.push(self.cfg.ssid.len() as u8); f.extend_from_slice(self.cfg.ssid.as_bytes());
        f.extend_from_slice(&[1, 8, 0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24]);      // supported rates 1 2 5.5 11 (basic) 6 9 12 18
        f.extend_from_slice(&[3, 1, self.cfg.channel]);                                     // DS parameter set
        f.extend_from_slice(&[5, 4, 0, 1, 0, 0]);                                           // TIM
        f.extend_from_slice(&[7, 6, b'S', b'E', b' ', 1, 13, 20]);                          // country
        f.extend_from_slice(&[50, 4, 0x30, 0x48, 0x60, 0x6c]);                              // extended rates 24 36 48 54
        if self.cfg.psk.is_some() { f.extend_from_slice(RSN_IE); }                          // RSN: WPA2-PSK, CCMP
    }
    fn beacon_like(&mut self, subtype: u16, dst: &[u8; 6], now_us: u64) -> Vec<u8> {
        let bssid = self.cfg.bssid;
        let mut f = self.hdr(subtype << 4, dst, &bssid);
        f.extend_from_slice(&now_us.to_le_bytes());                                         // timestamp
        f.extend_from_slice(&100u16.to_le_bytes());                                          // beacon interval (TU)
        f.extend_from_slice(&self.capability().to_le_bytes());
        self.common_ies(&mut f);
        f
    }
    fn send(&mut self, at_us: u64, frame: Vec<u8>) {
        if self.log { let d = describe(&frame); if d.contains("auth") || d.contains("assoc") || d.contains("888e") { eprintln!("[wifi] AP -> {}  hex={:02x?}", d, frame); } else { eprintln!("[wifi] AP -> {} (t+{} us)", d, at_us); } }
        self.queue.push(AirFrame { at_us, frame });
    }
    /// Time-driven behaviour (beacons). Returns frames due at or before `now_us`.
    pub fn step(&mut self, now_us: u64) -> Vec<AirFrame> {
        let mgmt_pending = self.queue.iter().any(|a| !is_beacon(&a.frame));
        if now_us >= self.next_beacon_us && !mgmt_pending {
            self.next_beacon_us += self.beacon_interval_us;
            if self.next_beacon_us <= now_us { self.next_beacon_us = now_us + self.beacon_interval_us; }
            let b = self.beacon_like(8, &[0xff; 6], now_us); self.stats.0 += 1;
            self.queue.push(AirFrame { at_us: now_us, frame: b });
        }
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.queue).into_iter().partition(|a| a.at_us <= now_us);
        self.queue = later;
        due
    }
    /// The station transmitted `f` at `now_us`. Returns data frames (as 802.11) for the network backend.
    pub fn on_station_tx(&mut self, f: &[u8], now_us: u64) -> Option<Vec<u8>> {
        if f.len() < 24 { return None; }
        let fc = u16::from_le_bytes([f[0], f[1]]);
        let (ty, st) = ((fc >> 2) & 3, (fc >> 4) & 0xf);
        let mut a2 = [0u8; 6]; a2.copy_from_slice(&f[10..16]);
        let to_us = |a1: &[u8]| a1 == [0xff; 6] || a1 == self.cfg.bssid;
        match (ty, st) {
            (0, 4) => {                                                                      // probe request: for us or wildcard?
                let ssid_ok = ies(f, 24).iter().any(|(id, d)| *id == 0 && (d.is_empty() || *d == self.cfg.ssid.as_bytes()));
                if ssid_ok && to_us(&f[4..10]) { let r = self.beacon_like(5, &a2, now_us); self.stats.1 += 1; self.send(now_us + 1500, r); }
            }
            (0, 11) if f.len() >= 30 && to_us(&f[4..10]) => {                                // authentication (open system)
                let (alg, seq) = (u16::from_le_bytes([f[24], f[25]]), u16::from_le_bytes([f[26], f[27]]));
                if self.log { eprintln!("[wifi] station AUTH req alg={} seq={} status={} hex={:02x?}", alg, seq, u16::from_le_bytes([f[28],f[29]]), f); }
                if alg == 0 && seq == 1 {
                    self.sta = a2; self.state = StaState::Authenticated;
                    let mut r = self.hdr(11 << 4, &a2, &self.cfg.bssid.clone());
                    r.extend_from_slice(&[0, 0, 2, 0, 0, 0]);                                // open, seq 2, status success
                    self.send(now_us + 300, r);
                }
            }
            (0, 0) | (0, 2) if to_us(&f[4..10]) && self.state != StaState::Idle => {         // (re)association request
                // Reassociation (subtype 2) is answered with a reassociation response
                // (subtype 3). An association response here is a different frame and the
                // driver ignores it.
                let response = if st == 2 { 3 } else { 1 };
                self.state = StaState::Associated;
                self.associations = self.associations.wrapping_add(1);
                let mut r = self.hdr(response << 4, &a2, &self.cfg.bssid.clone());
                r.extend_from_slice(&self.capability().to_le_bytes()); r.extend_from_slice(&[0, 0]); r.extend_from_slice(&(0xc000 | self.aid).to_le_bytes());
                r.extend_from_slice(&[1, 8, 0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24]); r.extend_from_slice(&[50, 4, 0x30, 0x48, 0x60, 0x6c]);
                self.send(now_us + 300, r);
                if self.cfg.psk.is_some() {                       // WPA2: start the four-way handshake
                    self.forget_keys();
                    self.wpa.replay += 1;
                    let anonce = self.wpa.anonce;
                    let m1 = self.eapol(0x008a, anonce, &[], None);
                    self.send(now_us + 30_000, m1);
                    self.wpa.state = WpaState::AwaitingMessage2;
                }
            }
            (0, 12) if to_us(&f[4..10]) => { self.state = StaState::Idle; self.forget_keys(); }
            // Disassociation ends the association only. The station stays authenticated
            // and may reassociate without a new open-system exchange.
            (0, 10) if to_us(&f[4..10]) => {
                if self.state == StaState::Associated { self.state = StaState::Authenticated; }
                self.forget_keys();
            }
            // Data to the DS from a station this AP has no association for (a quick boot restored
            // only the guest's side): deauthenticate it, reason 7 "class 3 frame from a
            // nonassociated station", so its driver reconnects, as a real AP would. At most
            // once per 100 ms, since a station keeps sending until the deauth lands.
            (2, _) if self.state != StaState::Associated && f[4..10] == self.cfg.bssid && fc & 0x0100 != 0 => {
                if self.class3_deauth_us.is_none_or(|at| now_us.wrapping_sub(at) >= 100_000) {
                    self.class3_deauth_us = Some(now_us);
                    self.state = StaState::Idle;
                    self.forget_keys();
                    let bssid = self.cfg.bssid;
                    let mut frame = self.hdr(12 << 4, &a2, &bssid);
                    frame.extend_from_slice(&7u16.to_le_bytes());
                    self.send(now_us + 300, frame);
                    if self.log { eprintln!("[wifi] data from non-associated station {}: deauthenticated (reason 7)", mac_str(&a2)); }
                }
                return None;
            }
            (2, _) if self.state == StaState::Associated && f[4..10] == self.cfg.bssid => {  // data to the DS
                if st == 4 || st == 12 { return None; }                                       // null frames (power save)
                let hdr = if st & 8 != 0 { 26 } else { 24 };
                // Protected frame: the MAC would have encrypted in place, so the descriptor holds
                // plaintext framed by an 8-byte CCMP header and 8 bytes of MIC space. Take those off.
                let plain;
                let f: &[u8] = if fc & 0x4000 != 0 && f.len() > hdr + 16 {
                    let mut v = Vec::with_capacity(f.len() - 16);
                    v.extend_from_slice(&f[..hdr]);
                    v.extend_from_slice(&f[hdr + 8..f.len() - 8]);
                    v[1] &= !0x40;                                                             // clear the protected bit
                    plain = v; &plain
                } else { f };
                if f.len() > hdr + 8 && f[hdr] == 0xaa && f[hdr + 6] == 0x88 && f[hdr + 7] == 0x8e {
                    self.on_eapol(&f[hdr + 8..], now_us);
                    return None;
                }
                self.stats.2 += 1; return Some(f.to_vec());
            }
            _ => {}
        }
        None
    }
    /// Build an EAPOL-Key frame (802.1X over LLC/SNAP in an 802.11 data frame from the DS).
    fn eapol(&mut self, key_info: u16, nonce: [u8; 32], key_data: &[u8], mic_key: Option<&[u8]>) -> Vec<u8> {
        let mut body = Vec::with_capacity(99 + key_data.len());
        body.push(2);                                                    // 802.1X-2004
        body.push(3);                                                    // EAPOL-Key
        body.extend_from_slice(&((95 + key_data.len()) as u16).to_be_bytes());
        body.push(2);                                                    // RSN key descriptor
        body.extend_from_slice(&key_info.to_be_bytes());
        body.extend_from_slice(&16u16.to_be_bytes());                    // key length (CCMP)
        body.extend_from_slice(&self.wpa.replay.to_be_bytes());
        body.extend_from_slice(&nonce);
        body.extend_from_slice(&[0u8; 16]);                              // key IV
        body.extend_from_slice(&[0u8; 8]);                               // key RSC
        body.extend_from_slice(&[0u8; 8]);                               // key ID
        let mic_at = body.len();
        body.extend_from_slice(&[0u8; 16]);
        body.extend_from_slice(&(key_data.len() as u16).to_be_bytes());
        body.extend_from_slice(key_data);
        if let Some(kck) = mic_key {
            let m = esp_periph::crypto::hmac_sha1(kck, &body);
            body[mic_at..mic_at + 16].copy_from_slice(&m[..16]);
        }
        let sta = self.sta; let bssid = self.cfg.bssid;
        let mut f = self.hdr(0x0208, &sta, &bssid);                      // data, from-DS
        f[16..22].copy_from_slice(&bssid);
        f.extend_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0, 0x88, 0x8e]);
        f.extend_from_slice(&body);
        f
    }

    /// Handle an EAPOL-Key frame from the station (messages 2 and 4 of the handshake).
    fn on_eapol(&mut self, body: &[u8], now_us: u64) {
        if body.len() < 99 || body[1] != 3 { return; }
        // the MIC covers exactly the 802.1X frame; the 802.11 payload can carry trailing bytes
        let n = 4 + u16::from_be_bytes([body[2], body[3]]) as usize;
        let Some(body) = body.get(..n).filter(|b| b.len() >= 99) else { return; };
        let key_data_len = u16::from_be_bytes([body[97], body[98]]) as usize;
        if 99 + key_data_len != body.len() { return; }
        let key_info = u16::from_be_bytes([body[5], body[6]]);
        let has_mic = key_info & 0x0100 != 0;
        let secure = key_info & 0x0200 != 0;
        if !has_mic { return; }
        if !secure && self.wpa.state == WpaState::AwaitingMessage2 {
            // message 2: take the SNonce and derive the pairwise key
            let mut snonce = [0u8; 32]; snonce.copy_from_slice(&body[17..49]);
            let (aa, spa) = (self.cfg.bssid, self.sta);
            let (lo_mac, hi_mac) = if aa <= spa { (aa, spa) } else { (spa, aa) };
            let (an, sn) = (self.wpa.anonce, snonce);
            let (lo_n, hi_n) = if an <= sn { (an, sn) } else { (sn, an) };
            let mut data = Vec::with_capacity(76);
            data.extend_from_slice(&lo_mac); data.extend_from_slice(&hi_mac);
            data.extend_from_slice(&lo_n); data.extend_from_slice(&hi_n);
            let ptk = esp_periph::crypto::prf(&self.wpa.pmk, "Pairwise key expansion", &data, 384);
            // self-check: recompute the station's own MIC over message 2. If this matches, the PMK,
            // the PTK derivation and the MIC scope are all right and any later failure is elsewhere.
            {
                let mut probe = body.to_vec();
                let mic_at = 81;
                let mut recv = [0u8; 16]; recv.copy_from_slice(&probe[mic_at..mic_at + 16]);
                for b in probe[mic_at..mic_at + 16].iter_mut() { *b = 0; }
                let calc = esp_periph::crypto::hmac_sha1(&ptk[0..16], &probe);
                if self.log {
                    eprintln!("[wifi] WPA2 msg2: PTK derived, station MIC {} (recv {:02x?} calc {:02x?})",
                              if calc[..16] == recv { "VERIFIED" } else { "MISMATCH" }, &recv[..4], &calc[..4]);
                }
            }

            // message 3: RSN IE + the group key, wrapped with the KEK
            let mut kd = Vec::new();
            kd.extend_from_slice(RSN_IE);
            kd.extend_from_slice(&[0xdd, 22, 0x00, 0x0f, 0xac, 0x01, 0x01, 0x00]);   // GTK KDE, key id 1
            kd.extend_from_slice(&self.wpa.gtk);
            if kd.len() % 8 != 0 { kd.push(0xdd); while kd.len() % 8 != 0 { kd.push(0); } }   // pad: one 0xDD then zeros
            let mut kek = [0u8; 16]; kek.copy_from_slice(&ptk[16..32]);
            let wrapped = esp_periph::crypto::aes_key_wrap(&kek, &kd);
            self.wpa.replay += 1;
            let anonce = self.wpa.anonce;
            let m3 = self.eapol(0x13ca, anonce, &wrapped, Some(&ptk[..16]));
            self.send(now_us + 2_000, m3);
            self.wpa.state = WpaState::AwaitingMessage4;
        } else if secure && self.wpa.state == WpaState::AwaitingMessage4 {
            self.wpa.state = WpaState::Installed;
            if self.log { eprintln!("[wifi] WPA2 four-way handshake complete"); }
        }
    }

    /// Wrap an Ethernet frame from the network backend into an 802.11 data frame from the DS.
    pub fn data_from_ds(&mut self, eth: &[u8]) -> Option<Vec<u8>> {
        if self.state != StaState::Associated || eth.len() < 14 { return None; }
        let mut dst = [0u8; 6]; dst.copy_from_slice(&eth[0..6]); let mut src = [0u8; 6]; src.copy_from_slice(&eth[6..12]);
        let bssid = self.cfg.bssid;
        // Once the keys are installed the frame must look encrypted: protected bit, CCMP header and
        // room for the MIC. The payload stays in the clear — as far as firmware is concerned the MAC
        // decrypted it in place.
        let protected = if self.wpa.state == WpaState::Installed { 0x4000 } else { 0 };
        let ccmp_hdr = protected != 0;
        let mut f = self.hdr((2 << 2) | 0x0200 | protected, &dst, &src);                        // data, from-DS
        f[16..22].copy_from_slice(&src); f[10..16].copy_from_slice(&bssid);
        if ccmp_hdr {
            // CCMP header, as the hardware would leave it after decrypting in place
            self.pn += 1;
            let pn = self.pn;
            let keyid = if dst[0] & 1 != 0 { 1 } else { 0 };                                    // group frames use the GTK
            f.extend_from_slice(&[pn as u8, (pn >> 8) as u8, 0, 0x20 | (keyid << 6),
                                  (pn >> 16) as u8, (pn >> 24) as u8, (pn >> 32) as u8, (pn >> 40) as u8]);
        }
        f.extend_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0]); f.extend_from_slice(&eth[12..14]);  // LLC/SNAP
        f.extend_from_slice(&eth[14..]);
        if ccmp_hdr { f.extend_from_slice(&[0u8; 8]); }                                         // MIC space
        Some(f)
    }
}

/// 802.11 data frame (to the DS) -> Ethernet frame.
pub fn data_to_eth(f: &[u8]) -> Option<Vec<u8>> {
    if f.len() < 2 { return None; }
    let fc = u16::from_le_bytes([f[0], f[1]]); let st = (fc >> 4) & 0xf;
    let hdr = if st & 8 != 0 { 26 } else { 24 };
    if f.len() < hdr + 8 || f[hdr] != 0xaa { return None; }
    let mut e = Vec::with_capacity(f.len());
    e.extend_from_slice(&f[16..22]);   // dst = addr3
    e.extend_from_slice(&f[10..16]);   // src = addr2
    e.extend_from_slice(&f[hdr + 6..hdr + 8]);
    e.extend_from_slice(&f[hdr + 8..]);
    Some(e)
}

/// Emulated station that joins a SoftAP the *guest* is running (G2b / E8).
///
/// The virtual AP above is the network the guest joins. This peer is the other
/// role: it hears the guest's beacons, authenticates and associates as a
/// station, takes a DHCP lease, and bridges Ethernet both ways. A protected
/// BSS is joined only when a passphrase is set; the four-way handshake is the
/// supplicant side of the same EAPOL the virtual AP speaks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PeerState { Idle, WaitAuth, WaitAssoc, WaitHandshake, Associated }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DhcpState { Idle, Discover, Request, Bound }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Supplicant { Idle, WaitM3, Installed }

pub struct StationPeer {
    pub mac: [u8; 6],
    state: PeerState,
    pub ssid: String,
    pub bssid: [u8; 6],
    pub privacy: bool,
    /// DS-parameter channel from the beacon. RX frames we send back are tagged with this.
    pub channel: u8,
    pub ip: Option<[u8; 4]>,
    pub server_ip: [u8; 4],
    seq: u16,
    aid: u16,
    dhcp: DhcpState,
    xid: u32,
    queue: Vec<AirFrame>,
    /// Ethernet the guest AP sent this station, excluding DHCP the peer consumed.
    /// Kept off the station NAT (10.0.2.0/24).
    pub eth_out: Vec<Vec<u8>>,
    passphrase: Option<String>,
    pmk: Option<[u8; 32]>,
    anonce: [u8; 32],
    snonce: [u8; 32],
    kck: [u8; 16],
    kek: [u8; 16],
    gtk: [u8; 16],
    replay: u64,
    supplicant: Supplicant,
    rsn: Vec<u8>,
    pn: u64,
    last_tx_us: u64,
}

impl StationPeer {
    pub fn new(mac: [u8; 6]) -> Self {
        StationPeer {
            mac, state: PeerState::Idle, ssid: String::new(), bssid: [0; 6], privacy: false, channel: 1,
            ip: None, server_ip: [0; 4], seq: 1, aid: 0, dhcp: DhcpState::Idle, xid: 0x4e4f_5401,
            queue: Vec::new(), eth_out: Vec::new(), passphrase: None, pmk: None, anonce: [0; 32],
            snonce: [0; 32], kck: [0; 16], kek: [0; 16], gtk: [0; 16], replay: 0,
            supplicant: Supplicant::Idle, rsn: Vec::new(), pn: 0, last_tx_us: 0,
        }
    }

    /// WPA2-PSK passphrase for a protected BSS. Empty means leave protected networks alone.
    pub fn set_passphrase(&mut self, psk: &str) {
        if psk.is_empty() {
            self.passphrase = None;
        } else {
            self.passphrase = Some(psk.to_string());
        }
        self.pmk = None;
    }

    pub fn has_passphrase(&self) -> bool { self.passphrase.is_some() }

    /// The AP disappeared without a deauth (the chip reset). The passphrase stays;
    /// the association does not, so the next beacon starts a new join.
    pub fn release_link(&mut self) {
        self.state = PeerState::Idle;
        self.supplicant = Supplicant::Idle;
        self.dhcp = DhcpState::Idle;
        self.ip = None;
        self.ssid.clear();
        self.bssid = [0; 6];
        self.privacy = false;
        self.aid = 0;
        self.pmk = None;
        self.queue.clear();
        self.eth_out.clear();
        self.rsn.clear();
        self.kck = [0; 16];
        self.kek = [0; 16];
        self.gtk = [0; 16];
        self.replay = 0;
        self.pn = 0;
        self.last_tx_us = 0;
    }

    pub fn phase(&self) -> &'static str {
        match self.state {
            PeerState::Idle => "idle",
            PeerState::WaitAuth => "auth",
            PeerState::WaitAssoc => "assoc",
            PeerState::WaitHandshake => match self.supplicant {
                Supplicant::WaitM3 => "handshake-m3",
                Supplicant::Installed => "keys",
                Supplicant::Idle => "handshake",
            },
            PeerState::Associated => if self.ip.is_some() { "leased" } else { "associated" },
        }
    }

    /// Watch one frame the guest transmitted. Management responses and DHCP
    /// are queued for delivery back onto the guest's RX ring.
    pub fn observe(&mut self, frame: &[u8], now_us: u64) {
        if frame.len() < 24 { return; }
        let fc = u16::from_le_bytes([frame[0], frame[1]]);
        let (ty, st) = ((fc >> 2) & 3, (fc >> 4) & 0xf);
        let to_us = frame[4..10] == self.mac || frame[4..10] == [0xff; 6];
        match (ty, st) {
            (0, 8) | (0, 5) => self.see_beacon(frame, now_us),
            (0, 11) if self.state == PeerState::WaitAuth && to_us && frame.len() >= 30 => {
                let seq = u16::from_le_bytes([frame[26], frame[27]]);
                let status = u16::from_le_bytes([frame[28], frame[29]]);
                if seq == 2 && status == 0 {
                    self.send_assoc(now_us);
                }
            }
            (0, 1) if self.state == PeerState::WaitAssoc && to_us && frame.len() >= 30 => {
                let status = u16::from_le_bytes([frame[26], frame[27]]);
                if status == 0 {
                    self.aid = u16::from_le_bytes([frame[28], frame[29]]) & 0x3fff;
                    if self.privacy {
                        self.state = PeerState::WaitHandshake;
                        self.supplicant = Supplicant::Idle;
                    } else {
                        self.state = PeerState::Associated;
                        self.send_dhcp_discover(now_us);
                    }
                }
            }
            (0, 12) | (0, 10) if to_us => {
                self.state = PeerState::Idle;
                self.supplicant = Supplicant::Idle;
                self.dhcp = DhcpState::Idle;
                self.ip = None;
            }
            (2, _) if matches!(self.state, PeerState::WaitHandshake | PeerState::Associated) && fc & 0x0300 == 0x0200 => self.on_from_ds(frame, now_us),
            _ => {}
        }
    }

    pub fn take_due(&mut self, now_us: u64) -> Vec<AirFrame> {
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.queue).into_iter().partition(|a| a.at_us <= now_us);
        self.queue = later;
        due
    }

    /// Put frames back that the air could not deliver this step. They stay on this
    /// peer, not on the virtual AP the guest station joins.
    pub fn requeue(&mut self, frames: Vec<AirFrame>) {
        self.queue.extend(frames);
    }

    pub fn take_eth(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.eth_out)
    }

    /// Ethernet the runtime wants to deliver to the guest AP, as a station data frame.
    pub fn inject_eth(&mut self, eth: &[u8], now_us: u64) {
        if self.state != PeerState::Associated || eth.len() < 14 { return; }
        let mut dst = [0u8; 6]; dst.copy_from_slice(&eth[0..6]);
        let frame = self.data_to_ap(&dst, &eth[12..]);
        self.queue.push(AirFrame { at_us: now_us, frame });
    }

    fn see_beacon(&mut self, frame: &[u8], now_us: u64) {
        let body = if (frame[0] >> 4) & 0xf == 8 || (frame[0] >> 4) & 0xf == 5 { 36 } else { return };
        if frame.len() < body { return; }
        let mut bssid = [0u8; 6];
        bssid.copy_from_slice(&frame[16..22]);
        let ssid = ies(frame, body).into_iter().find(|(id, _)| *id == 0).map(|(_, d)| String::from_utf8_lossy(d).into_owned()).unwrap_or_default();
        if ssid.is_empty() { return; }
        let cap = u16::from_le_bytes([frame[34], frame[35]]);
        let rsn = ies(frame, body).into_iter().find(|(id, _)| *id == 48).map(|(id, data)| {
            let mut ie = vec![id, data.len() as u8];
            ie.extend_from_slice(data);
            ie
        }).unwrap_or_else(|| RSN_IE.to_vec());
        let privacy = cap & 0x0010 != 0;
        let channel = ies(frame, body).into_iter().find(|(id, data)| *id == 3 && data.len() == 1).map(|(_, data)| data[0]).unwrap_or(1);
        if self.state == PeerState::Idle {
            if self.ssid != ssid { self.pmk = None; }
            self.bssid = bssid;
            self.ssid = ssid;
            self.privacy = privacy;
            self.channel = channel;
            self.rsn = rsn;
            if self.privacy && self.passphrase.is_none() { return; }
            self.send_auth(now_us);
            return;
        }
        // The first auth can be dropped before the guest's RX ring is up. Retransmit
        // while the AP keeps beaconing, but not on every beacon.
        if self.bssid != bssid || now_us.saturating_sub(self.last_tx_us) < 100_000 { return; }
        self.rsn = rsn;
        match self.state {
            PeerState::WaitAuth => self.send_auth(now_us),
            PeerState::WaitAssoc => self.send_assoc(now_us),
            _ => {}
        }
    }

    fn send_auth(&mut self, now_us: u64) {
        let bssid = self.bssid;
        let mut frame = self.mgmt(11 << 4, &bssid);
        frame.extend_from_slice(&0u16.to_le_bytes()); // open system
        frame.extend_from_slice(&1u16.to_le_bytes()); // transaction
        frame.extend_from_slice(&0u16.to_le_bytes()); // status
        self.state = PeerState::WaitAuth;
        self.last_tx_us = now_us;
        self.queue.push(AirFrame { at_us: now_us + 1_000, frame });
    }

    fn send_assoc(&mut self, now_us: u64) {
        let bssid = self.bssid;
        let mut frame = self.mgmt(0, &bssid);
        frame.extend_from_slice(&0x0001u16.to_le_bytes()); // ESS
        frame.extend_from_slice(&10u16.to_le_bytes());     // listen interval
        frame.push(0);
        frame.push(self.ssid.len() as u8);
        frame.extend_from_slice(self.ssid.as_bytes());
        frame.extend_from_slice(&[1, 8, 0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24]);
        if self.privacy { frame.extend_from_slice(&self.rsn); }
        self.state = PeerState::WaitAssoc;
        self.last_tx_us = now_us;
        self.queue.push(AirFrame { at_us: now_us + 1_000, frame });
    }

    fn send_dhcp_discover(&mut self, now_us: u64) {
        self.dhcp = DhcpState::Discover;
        let pkt = dhcp_client(&self.mac, self.xid, 1, None, None);
        let frame = self.data_to_ap(&[0xff; 6], &pkt);
        self.queue.push(AirFrame { at_us: now_us + 5_000, frame });
    }

    fn on_from_ds(&mut self, frame: &[u8], now_us: u64) {
        let Some(eth) = from_ds_to_eth(frame) else { return };
        if eth.len() >= 14 && eth[12] == 0x88 && eth[13] == 0x8e {
            self.on_eapol(&eth[14..], now_us);
            return;
        }
        if self.state != PeerState::Associated { return; }
        if let Some((kind, yiaddr, server)) = dhcp_reply(&eth) {
            match (self.dhcp, kind) {
                (DhcpState::Discover, 2) => {
                    self.server_ip = server;
                    self.dhcp = DhcpState::Request;
                    let pkt = dhcp_client(&self.mac, self.xid, 3, Some(yiaddr), Some(server));
                    let tx = self.data_to_ap(&[0xff; 6], &pkt);
                    self.queue.push(AirFrame { at_us: now_us + 1_000, frame: tx });
                }
                (DhcpState::Request, 5) => {
                    self.ip = Some(yiaddr);
                    self.dhcp = DhcpState::Bound;
                }
                _ => {}
            }
            return;
        }
        self.eth_out.push(eth);
    }

    fn mgmt(&mut self, fc: u16, bssid: &[u8; 6]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(64);
        frame.extend_from_slice(&fc.to_le_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(bssid);
        frame.extend_from_slice(&self.mac);
        frame.extend_from_slice(bssid);
        frame.extend_from_slice(&(self.seq << 4).to_le_bytes());
        self.seq = self.seq.wrapping_add(1);
        frame
    }

    /// To-DS data carrying an Ethernet payload starting at the ethertype.
    /// After the handshake the frame is plaintext framed as CCMP, which is what
    /// firmware sees when the MAC encrypts in place.
    fn data_to_ap(&mut self, dst: &[u8; 6], eth_from_type: &[u8]) -> Vec<u8> {
        self.frame_to_ap(dst, eth_from_type, self.supplicant == Supplicant::Installed)
    }

    fn frame_to_ap(&mut self, dst: &[u8; 6], eth_from_type: &[u8], protect: bool) -> Vec<u8> {
        let fc: u16 = (2 << 2) | 0x0100 | if protect { 0x4000 } else { 0 };
        let mut frame = Vec::with_capacity(40 + eth_from_type.len());
        frame.extend_from_slice(&fc.to_le_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&self.bssid);
        frame.extend_from_slice(&self.mac);
        frame.extend_from_slice(dst);
        frame.extend_from_slice(&(self.seq << 4).to_le_bytes());
        self.seq = self.seq.wrapping_add(1);
        if protect {
            self.pn += 1;
            let pn = self.pn;
            frame.extend_from_slice(&[pn as u8, (pn >> 8) as u8, 0, 0x20, (pn >> 16) as u8, (pn >> 24) as u8, (pn >> 32) as u8, (pn >> 40) as u8]);
        }
        frame.extend_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0]);
        frame.extend_from_slice(eth_from_type);
        if protect { frame.extend_from_slice(&[0; 8]); }
        frame
    }

    fn on_eapol(&mut self, raw: &[u8], now_us: u64) {
        if raw.len() < 99 || raw[1] != 3 { return; }
        let declared = 4 + u16::from_be_bytes([raw[2], raw[3]]) as usize;
        let Some(body) = raw.get(..declared).filter(|b| b.len() >= 99) else { return };
        let key_data_len = u16::from_be_bytes([body[97], body[98]]) as usize;
        if 99 + key_data_len != body.len() { return; }
        let key_info = u16::from_be_bytes([body[5], body[6]]);
        let ack = key_info & 0x0080 != 0;
        let has_mic = key_info & 0x0100 != 0;
        let secure = key_info & 0x0200 != 0;
        let encrypted = key_info & 0x1000 != 0;
        self.replay = u64::from_be_bytes(body[9..17].try_into().unwrap());
        if ack && !has_mic && !secure && self.state == PeerState::WaitHandshake {
            self.anonce.copy_from_slice(&body[17..49]);
            self.send_message2(now_us);
        } else if ack && has_mic && secure && encrypted && self.supplicant == Supplicant::WaitM3 {
            if !self.mic_ok(body) { return; }
            let wrapped = &body[99..];
            let Some(plain) = esp_periph::crypto::aes_key_unwrap(&self.kek, wrapped) else { return; };
            if let Some(gtk) = gtk_from_key_data(&plain) { self.gtk = gtk; }
            self.send_message4(now_us);
            self.supplicant = Supplicant::Installed;
            self.state = PeerState::Associated;
            self.send_dhcp_discover(now_us);
        }
    }

    fn send_message2(&mut self, now_us: u64) {
        self.ensure_pmk();
        if self.pmk.is_none() { return; }
        let seed = esp_periph::crypto::sha1(&[&self.mac[..], self.ssid.as_bytes()].concat());
        for i in 0..32 { self.snonce[i] = seed[i % 20] ^ i as u8 ^ 0x3c; }
        self.derive_ptk();
        let nonce = self.snonce;
        // Message 2 carries the same RSN element as the association request.
        // An empty key-data field is classified as message 4 and ignored.
        let rsn = self.rsn.clone();
        let frame = self.eapol_frame(0x010a, &nonce, &rsn, true);
        self.supplicant = Supplicant::WaitM3;
        self.queue.push(AirFrame { at_us: now_us + 1_000, frame });
    }

    fn send_message4(&mut self, now_us: u64) {
        let nonce = self.snonce;
        let frame = self.eapol_frame(0x030a, &nonce, &[], true);
        self.queue.push(AirFrame { at_us: now_us + 1_000, frame });
    }

    fn ensure_pmk(&mut self) {
        if self.pmk.is_some() { return; }
        let Some(psk) = self.passphrase.clone() else { return };
        let derived = esp_periph::crypto::pbkdf2_sha1(psk.as_bytes(), self.ssid.as_bytes(), 4096, 32);
        let mut pmk = [0u8; 32];
        pmk.copy_from_slice(&derived[..32]);
        self.pmk = Some(pmk);
    }

    fn derive_ptk(&mut self) {
        let (aa, spa) = (self.bssid, self.mac);
        let (lo_mac, hi_mac) = if aa <= spa { (aa, spa) } else { (spa, aa) };
        let (an, sn) = (self.anonce, self.snonce);
        let (lo_n, hi_n) = if an <= sn { (an, sn) } else { (sn, an) };
        let mut data = Vec::with_capacity(76);
        data.extend_from_slice(&lo_mac);
        data.extend_from_slice(&hi_mac);
        data.extend_from_slice(&lo_n);
        data.extend_from_slice(&hi_n);
        let pmk = self.pmk.unwrap_or([0; 32]);
        let ptk = esp_periph::crypto::prf(&pmk, "Pairwise key expansion", &data, 384);
        self.kck.copy_from_slice(&ptk[..16]);
        self.kek.copy_from_slice(&ptk[16..32]);
    }

    fn mic_ok(&self, body: &[u8]) -> bool {
        let mut probe = body.to_vec();
        let mut recv = [0u8; 16];
        recv.copy_from_slice(&probe[81..97]);
        probe[81..97].fill(0);
        let calc = esp_periph::crypto::hmac_sha1(&self.kck, &probe);
        calc[..16] == recv
    }

    fn eapol_frame(&mut self, key_info: u16, nonce: &[u8; 32], key_data: &[u8], with_mic: bool) -> Vec<u8> {
        let mut body = Vec::with_capacity(99 + key_data.len());
        body.push(2);
        body.push(3);
        body.extend_from_slice(&((95 + key_data.len()) as u16).to_be_bytes());
        body.push(2);
        body.extend_from_slice(&key_info.to_be_bytes());
        body.extend_from_slice(&16u16.to_be_bytes());
        body.extend_from_slice(&self.replay.to_be_bytes());
        body.extend_from_slice(nonce);
        body.extend_from_slice(&[0u8; 16]);
        body.extend_from_slice(&[0u8; 8]);
        body.extend_from_slice(&[0u8; 8]);
        let mic_at = body.len();
        body.extend_from_slice(&[0u8; 16]);
        body.extend_from_slice(&(key_data.len() as u16).to_be_bytes());
        body.extend_from_slice(key_data);
        if with_mic {
            let mic = esp_periph::crypto::hmac_sha1(&self.kck, &body);
            body[mic_at..mic_at + 16].copy_from_slice(&mic[..16]);
        }
        let mut payload = vec![0x88, 0x8e];
        payload.extend_from_slice(&body);
        let bssid = self.bssid;
        self.frame_to_ap(&bssid, &payload, false)
    }
}

fn gtk_from_key_data(plain: &[u8]) -> Option<[u8; 16]> {
    let mut i = 0;
    while i + 8 <= plain.len() {
        if plain[i] == 0xdd && i + 24 <= plain.len() && plain[i + 1] >= 22 && plain[i + 2..i + 8] == [0x00, 0x0f, 0xac, 0x01, 0x01, 0x00] {
            let mut gtk = [0u8; 16];
            gtk.copy_from_slice(&plain[i + 8..i + 24]);
            return Some(gtk);
        }
        if plain[i] == 0xdd {
            i += 2 + plain[i + 1] as usize;
        } else {
            i += 1;
        }
    }
    None
}

fn from_ds_to_eth(frame: &[u8]) -> Option<Vec<u8>> {
    let fc = u16::from_le_bytes([frame[0], frame[1]]);
    let st = (fc >> 4) & 0xf;
    let mut hdr = if st & 8 != 0 { 26 } else { 24 };
    let protected = fc & 0x4000 != 0;
    if protected { hdr += 8; }
    let end = if protected { frame.len().saturating_sub(8) } else { frame.len() };
    if end < hdr + 8 || frame.get(hdr) != Some(&0xaa) { return None; }
    let mut eth = Vec::with_capacity(end);
    eth.extend_from_slice(&frame[4..10]);   // dst = addr1
    eth.extend_from_slice(&frame[16..22]);  // src = addr3
    eth.extend_from_slice(&frame[hdr + 6..end]);
    Some(eth)
}

/// Ethernet payload beginning at the ethertype: IPv4/UDP DHCP from the client.
fn dhcp_client(mac: &[u8; 6], xid: u32, msg: u8, requested: Option<[u8; 4]>, server: Option<[u8; 4]>) -> Vec<u8> {
    let mut bootp = vec![0u8; 240];
    bootp[0] = 1; bootp[1] = 1; bootp[2] = 6;
    bootp[4..8].copy_from_slice(&xid.to_be_bytes());
    bootp[28..34].copy_from_slice(mac);
    bootp[236..240].copy_from_slice(&[0x63, 0x82, 0x53, 0x63]);
    bootp.extend_from_slice(&[53, 1, msg]);
    if let Some(ip) = requested { bootp.extend_from_slice(&[50, 4]); bootp.extend_from_slice(&ip); }
    if let Some(ip) = server { bootp.extend_from_slice(&[54, 4]); bootp.extend_from_slice(&ip); }
    bootp.push(255);
    let mut udp = Vec::new();
    udp.extend_from_slice(&68u16.to_be_bytes());
    udp.extend_from_slice(&67u16.to_be_bytes());
    udp.extend_from_slice(&((8 + bootp.len()) as u16).to_be_bytes());
    udp.extend_from_slice(&[0, 0]);
    udp.extend_from_slice(&bootp);
    // Pseudo-header: src 0.0.0.0, dst 255.255.255.255, proto 17. A computed 0 is stored as 0xFFFF.
    let mut pseudo = [0u8, 0, 0, 0, 255, 255, 255, 255, 0, 17, 0, 0];
    pseudo[10..12].copy_from_slice(&(udp.len() as u16).to_be_bytes());
    let mut covered = pseudo.to_vec();
    covered.extend_from_slice(&udp);
    let mut udp_sum = inet_checksum(&covered);
    if udp_sum == 0 { udp_sum = 0xffff; }
    udp[6..8].copy_from_slice(&udp_sum.to_be_bytes());
    let mut ip = Vec::new();
    ip.extend_from_slice(&[0x45, 0]);
    ip.extend_from_slice(&((20 + udp.len()) as u16).to_be_bytes());
    ip.extend_from_slice(&[0, 0, 0x40, 0, 64, 17, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255]);
    let ip_sum = inet_checksum(&ip);
    ip[10..12].copy_from_slice(&ip_sum.to_be_bytes());
    ip.extend_from_slice(&udp);
    let mut eth = Vec::new();
    eth.extend_from_slice(&[0x08, 0x00]);
    eth.extend_from_slice(&ip);
    eth
}

fn inet_checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() { sum += (data[i] as u32) << 8; }
    while sum >> 16 != 0 { sum = (sum & 0xffff) + (sum >> 16); }
    !(sum as u16)
}

/// (message type, yiaddr, server id) from an Ethernet frame, if it is a DHCP reply.
fn dhcp_reply(eth: &[u8]) -> Option<(u8, [u8; 4], [u8; 4])> {
    if eth.len() < 14 + 20 + 8 + 240 || eth[12] != 0x08 || eth[13] != 0x00 { return None; }
    let ip = &eth[14..];
    if ip[9] != 17 { return None; }
    let ihl = ((ip[0] & 0xf) as usize) * 4;
    let udp = ip.get(ihl..)?;
    if udp.len() < 8 + 240 { return None; }
    let sport = u16::from_be_bytes([udp[0], udp[1]]);
    let dport = u16::from_be_bytes([udp[2], udp[3]]);
    if sport != 67 || dport != 68 { return None; }
    let bootp = &udp[8..];
    if bootp[0] != 2 || bootp.get(236..240) != Some(&[0x63, 0x82, 0x53, 0x63][..]) { return None; }
    let mut yiaddr = [0u8; 4];
    yiaddr.copy_from_slice(&bootp[16..20]);
    let mut kind = 0u8;
    let mut server = [0u8; 4];
    let mut i = 240;
    while i + 1 < bootp.len() {
        let (opt, len) = (bootp[i], bootp[i + 1] as usize);
        if opt == 255 { break; }
        if opt == 0 { i += 1; continue; }
        if i + 2 + len > bootp.len() { break; }
        if opt == 53 && len == 1 { kind = bootp[i + 2]; }
        if opt == 54 && len == 4 { server.copy_from_slice(&bootp[i + 2..i + 6]); }
        i += 2 + len;
    }
    (kind != 0).then_some((kind, yiaddr, server))
}

/// Snapshot restore hook (class X). Closes host TCP/UDP sockets and listeners.
/// Live TCP is not stored. WPA, `StationPeer`, and RX filter bits are not touched.
pub fn close_host_flows_for_restore(nat: &mut crate::nat::Nat) -> Vec<Vec<u8>> {
    nat.close_host_flows()
}

/// FCS (CRC-32) as the MAC appends it.
pub fn fcs(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data { crc ^= b as u32; for _ in 0..8 { crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 }; } }
    !crc
}
