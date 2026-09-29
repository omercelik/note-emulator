use super::*;

fn config() -> ApConfig {
    ApConfig { ssid: "esp32sim".into(), bssid: [2, 0x53, 0x49, 0x4d, 0, 1], channel: 6, psk: None }
}

fn station_frame(fc: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![0; 24];
    frame[..2].copy_from_slice(&fc.to_le_bytes());
    frame[4..10].copy_from_slice(&config().bssid);
    frame[10..16].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
    frame.extend_from_slice(payload);
    frame
}

#[test]
fn auth_rejects_every_truncated_fixed_body() {
    let valid = station_frame(11 << 4, &[0, 0, 1, 0, 0, 0]);
    for len in 0..valid.len() {
        let mut ap = VirtualAp::new(config(), true);
        assert!(ap.on_station_tx(&valid[..len], 0).is_none());
        assert_eq!(ap.state, StaState::Idle, "length {len}");
        assert!(ap.queue.is_empty(), "length {len}");
    }
    let mut ap = VirtualAp::new(config(), false);
    assert!(ap.on_station_tx(&valid, 0).is_none());
    assert_eq!(ap.state, StaState::Authenticated);
    assert_eq!(ap.queue.len(), 1);
}

#[test]
fn data_from_a_station_without_association_is_answered_with_a_deauth() {
    // A quick boot restores the guest as associated; this AP (a new process) has never seen it.
    let mut ap = VirtualAp::new(config(), false);
    let data = station_frame(0x0108, &[0xaa, 0xaa, 3, 0, 0, 0, 0x08, 0x00, 0x45]);
    assert!(ap.on_station_tx(&data, 1_000).is_none());
    assert_eq!(ap.queue.len(), 1);
    let deauth = &ap.queue[0].frame;
    assert_eq!(u16::from_le_bytes([deauth[0], deauth[1]]), 12 << 4);
    assert_eq!(deauth[4..10], [2, 3, 4, 5, 6, 7]);
    assert_eq!(u16::from_le_bytes([deauth[24], deauth[25]]), 7);
    // More data inside 100 ms does not repeat it; later it does.
    ap.on_station_tx(&data, 50_000);
    assert_eq!(ap.queue.len(), 1);
    ap.on_station_tx(&data, 101_000);
    assert_eq!(ap.queue.len(), 2);
    // An associated station's data is forwarded, never deauthenticated.
    let mut ap = VirtualAp::new(config(), false);
    ap.state = StaState::Associated;
    ap.sta = [2, 3, 4, 5, 6, 7];
    ap.on_station_tx(&data, 1_000);
    assert!(ap.queue.is_empty());
}

fn eapol_message4() -> Vec<u8> {
    let mut body = vec![0; 99];
    body[0] = 2;
    body[1] = 3;
    body[2..4].copy_from_slice(&95u16.to_be_bytes());
    body[4] = 2;
    body[5..7].copy_from_slice(&0x0300u16.to_be_bytes());
    body
}

fn send_eapol(ap: &mut VirtualAp, body: &[u8]) {
    ap.state = StaState::Associated;
    let mut payload = vec![0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e];
    payload.extend_from_slice(body);
    assert!(ap.on_station_tx(&station_frame(0x0108, &payload), 0).is_none());
}

#[test]
fn eapol_rejects_declared_lengths_shorter_than_key_header() {
    for len in 0..95u16 {
        let mut body = eapol_message4();
        body[2..4].copy_from_slice(&len.to_be_bytes());
        // Exercise the message-2 slices as well as the message-4 state transition.
        for (state, key_info) in [(WpaState::AwaitingMessage2, 0x0100u16), (WpaState::AwaitingMessage4, 0x0300)] {
            body[5..7].copy_from_slice(&key_info.to_be_bytes());
            let mut ap = VirtualAp::new(config(), false);
            ap.wpa.state = state;
            send_eapol(&mut ap, &body);
            assert_eq!(ap.wpa.state, state, "declared length {len}");
            assert!(ap.queue.is_empty());
        }
    }
}

#[test]
fn eapol_rejects_truncated_declared_payload_and_key_data() {
    for (declared, key_data) in [(96u16, 0u16), (95, 1), (u16::MAX, 0)] {
        let mut body = eapol_message4();
        body[2..4].copy_from_slice(&declared.to_be_bytes());
        body[97..99].copy_from_slice(&key_data.to_be_bytes());
        let mut ap = VirtualAp::new(config(), false);
        ap.wpa.state = WpaState::AwaitingMessage4;
        send_eapol(&mut ap, &body);
        assert_eq!(ap.wpa.state, WpaState::AwaitingMessage4);
        assert!(ap.queue.is_empty());
    }
}

fn beacon(ssid: &str, privacy: bool) -> Vec<u8> {
    let bssid = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
    let mut frame = vec![0u8; 36];
    frame[0] = 0x80; // beacon
    frame[4..10].copy_from_slice(&[0xff; 6]);
    frame[10..16].copy_from_slice(&bssid);
    frame[16..22].copy_from_slice(&bssid);
    let cap = if privacy { 0x0011u16 } else { 0x0001 };
    frame[34..36].copy_from_slice(&cap.to_le_bytes());
    frame.push(0);
    frame.push(ssid.len() as u8);
    frame.extend_from_slice(ssid.as_bytes());
    frame
}

fn mgmt_to_peer(subtype: u8, body: &[u8]) -> Vec<u8> {
    let mut frame = vec![0u8; 24];
    frame[0] = subtype << 4;
    frame[4..10].copy_from_slice(&[0x02, 0x4e, 0x4f, 0x54, 0x45, 0x01]);
    frame[10..16].copy_from_slice(&[0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
    frame[16..22].copy_from_slice(&[0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
    frame.extend_from_slice(body);
    frame
}

fn dhcp_offer(kind: u8) -> Vec<u8> {
    let mut bootp = vec![0u8; 240];
    bootp[0] = 2; bootp[1] = 1; bootp[2] = 6;
    bootp[4..8].copy_from_slice(&0x4e4f_5401u32.to_be_bytes());
    bootp[16..20].copy_from_slice(&[192, 168, 4, 2]);
    bootp[236..240].copy_from_slice(&[0x63, 0x82, 0x53, 0x63]);
    bootp.extend_from_slice(&[53, 1, kind, 54, 4, 192, 168, 4, 1, 255]);
    let mut udp = Vec::new();
    udp.extend_from_slice(&67u16.to_be_bytes());
    udp.extend_from_slice(&68u16.to_be_bytes());
    udp.extend_from_slice(&((8 + bootp.len()) as u16).to_be_bytes());
    udp.extend_from_slice(&[0, 0]);
    udp.extend_from_slice(&bootp);
    let mut ip = vec![0x45, 0, 0, 0, 0, 0, 0x40, 0, 64, 17, 0, 0, 192, 168, 4, 1, 255, 255, 255, 255];
    let total = (20 + udp.len()) as u16;
    ip[2..4].copy_from_slice(&total.to_be_bytes());
    ip.extend_from_slice(&udp);
    // from-DS data, addr1 = peer, addr3 = server mac
    let mut frame = vec![0u8; 24];
    frame[0] = 0x08;
    frame[1] = 0x02;
    frame[4..10].copy_from_slice(&[0x02, 0x4e, 0x4f, 0x54, 0x45, 0x01]);
    frame[10..16].copy_from_slice(&[0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
    frame[16..22].copy_from_slice(&[0x02, 0xaa, 0, 0, 0, 1]);
    frame.extend_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0, 0x08, 0x00]);
    frame.extend_from_slice(&ip);
    frame
}

#[test]
fn peer_joins_an_open_softap_and_takes_a_dhcp_lease() {
    let mut peer = StationPeer::new([0x02, 0x4e, 0x4f, 0x54, 0x45, 0x01]);
    peer.observe(&beacon("NOTE4C", true), 0);
    assert!(peer.queue.is_empty(), "a protected AP is not joined without a supplicant");
    assert!(peer.privacy);

    peer.observe(&beacon("NOTE4C", false), 0);
    let auth = peer.take_due(2_000);
    assert_eq!(auth.len(), 1);
    assert_eq!(auth[0].frame[0] >> 4, 11);
    assert_eq!(&auth[0].frame[4..10], &[0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
    assert_eq!(&auth[0].frame[10..16], &peer.mac);
    assert_eq!(peer.state, PeerState::WaitAuth);

    peer.observe(&mgmt_to_peer(11, &[0, 0, 2, 0, 0, 0]), 3_000);
    let assoc = peer.take_due(5_000);
    assert_eq!(assoc[0].frame[0] >> 4, 0);
    assert!(assoc[0].frame.windows(6).any(|w| w == b"NOTE4C"));
    assert_eq!(peer.state, PeerState::WaitAssoc);

    peer.observe(&mgmt_to_peer(1, &[0x01, 0, 0, 0, 0x01, 0xc0]), 6_000);
    assert_eq!(peer.state, PeerState::Associated);
    assert_eq!(peer.aid, 1);
    let discover = peer.take_due(20_000);
    assert_eq!(discover.len(), 1);
    assert_eq!(discover[0].frame[1] & 0x01, 0x01, "DHCP discover goes to the DS");
    let pkt = dhcp_client(&peer.mac, 0x4e4f_5401, 1, None, None);
    assert_eq!(inet_checksum(&pkt[2..22]), 0, "IPv4 header checksum");
    let udp = &pkt[22..];
    let mut pseudo = pkt[14..22].to_vec();
    pseudo.extend_from_slice(&[0, 17]);
    pseudo.extend_from_slice(&udp[4..6]);
    pseudo.extend_from_slice(udp);
    assert_eq!(inet_checksum(&pseudo), 0, "UDP checksum");

    peer.observe(&dhcp_offer(2), 21_000);
    let request = peer.take_due(30_000);
    assert_eq!(request.len(), 1);
    assert!(request[0].frame.windows(3).any(|w| w == [53, 1, 3]), "DHCP request");
    assert!(request[0].frame.windows(6).any(|w| w == [50, 4, 192, 168, 4, 2]));
    peer.observe(&dhcp_offer(5), 31_000);
    assert_eq!(peer.ip, Some([192, 168, 4, 2]));
    assert!(peer.take_eth().is_empty());

    let mut ip = vec![0u8; 14];
    ip[0..6].copy_from_slice(&peer.mac);
    ip[12] = 0x08; ip[13] = 0x00;
    ip.extend_from_slice(&[0x45, 0, 0, 20]);
    let mut data = vec![0u8; 24];
    data[0] = 0x08; data[1] = 0x02;
    data[4..10].copy_from_slice(&peer.mac);
    data[10..16].copy_from_slice(&[0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
    data[16..22].copy_from_slice(&[0x02, 0xaa, 0, 0, 0, 1]);
    data.extend_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0]);
    data.extend_from_slice(&ip[12..]);
    peer.observe(&data, 40_000);
    let out = peer.take_eth();
    assert_eq!(out.len(), 1);
    assert_eq!(&out[0][0..6], &peer.mac);

    peer.inject_eth(&ip, 41_000);
    let back = peer.take_due(41_000);
    assert_eq!(back.len(), 1);
    assert_eq!(&back[0].frame[4..10], &peer.bssid);
    assert_eq!(&back[0].frame[16..22], &ip[0..6]);
}

#[test]
fn peer_completes_wpa2_with_the_virtual_ap() {
    let mut cfg = config();
    cfg.psk = Some("esp32sim-pass".into());
    let mut ap = VirtualAp::new(cfg, false);
    let mut peer = StationPeer::new([0x02, 0x4e, 0x4f, 0x54, 0x45, 0x01]);
    peer.set_passphrase("esp32sim-pass");
    let beacon = ap.step(200_000);
    assert!(is_beacon(&beacon[0].frame));
    peer.observe(&beacon[0].frame, 200_000);
    let mut t = 200_000u64;
    let mut protected_dhcp = false;
    let mut echoed_rsn = false;
    for _ in 0..16 {
        t += 50_000;
        for frame in peer.take_due(t) {
            if frame.frame.get(1).is_some_and(|b| b & 0x40 == 0x40) { protected_dhcp = true; }
            if frame.frame.windows(2).any(|w| w == [0x88, 0x8e]) && frame.frame.windows(RSN_IE.len()).any(|w| w == RSN_IE) {
                echoed_rsn = true;
            }
            ap.on_station_tx(&frame.frame, t);
        }
        for frame in ap.step(t) {
            if !is_beacon(&frame.frame) { peer.observe(&frame.frame, t); }
        }
    }
    assert_eq!(ap.wpa.state, WpaState::Installed);
    assert_eq!(peer.supplicant, Supplicant::Installed);
    assert_eq!(peer.gtk, ap.wpa.gtk);
    assert!(protected_dhcp, "DHCP after the handshake is framed as CCMP");
    assert!(echoed_rsn, "message 2 repeats the RSN element from the association");
}

#[test]
fn eapol_allows_bytes_after_declared_payload() {
    let mut body = eapol_message4();
    body.extend_from_slice(&[0xff; 8]);
    let mut ap = VirtualAp::new(config(), false);
    ap.wpa.state = WpaState::AwaitingMessage4;
    send_eapol(&mut ap, &body);
    assert_eq!(ap.wpa.state, WpaState::Installed);
}

#[test]
fn wpa_handshake_installs_keys_only_after_message4() {
    let mut cfg = config();
    cfg.psk = Some("esp32sim-pass".into());
    let mut ap = VirtualAp::new(cfg, false);
    assert_eq!(ap.wpa.state, WpaState::Idle);
    ap.on_station_tx(&station_frame(11 << 4, &[0, 0, 1, 0, 0, 0]), 0);
    ap.on_station_tx(&station_frame(0, &[1, 0, 0, 0]), 0);
    assert_eq!(ap.wpa.state, WpaState::AwaitingMessage2);
    let m1 = &ap.queue.last().unwrap().frame[32..];
    assert_eq!(&m1[5..7], &0x008au16.to_be_bytes());
    assert_eq!(&m1[81..97], &[0; 16]);
    ap.queue.clear();

    let mut m2 = eapol_message4();
    m2[5..7].copy_from_slice(&0x010au16.to_be_bytes());
    m2[17..49].fill(0x5a);
    send_eapol(&mut ap, &m2);
    assert_eq!(ap.wpa.state, WpaState::AwaitingMessage4);
    let m3 = &ap.queue.last().unwrap().frame[32..];
    assert_eq!(&m3[5..7], &0x13cau16.to_be_bytes());
    assert_ne!(&m3[81..97], &[0; 16]);
    let ethernet = [0u8; 14];
    assert_eq!(ap.data_from_ds(&ethernet).unwrap()[1] & 0x40, 0);

    send_eapol(&mut ap, &eapol_message4());
    assert_eq!(ap.wpa.state, WpaState::Installed);
    let protected = ap.data_from_ds(&ethernet).unwrap();
    assert_eq!(protected[1] & 0x40, 0x40);
    assert_eq!(protected.len(), 24 + 8 + 8 + 8); // MAC, CCMP, LLC/SNAP and MIC
}

#[test]
fn data_to_eth_rejects_every_truncated_header() {
    let mut payload = vec![0xaa, 0xaa, 3, 0, 0, 0, 0x08, 0];
    payload.extend_from_slice(&[1, 2, 3]);
    for fc in [0x0108, 0x0188] {
        let mut frame = station_frame(fc, &[]);
        if fc == 0x0188 { frame.extend_from_slice(&[0; 2]); }
        let header_len = frame.len() + 8;
        frame.extend_from_slice(&payload);
        for len in 0..header_len {
            assert!(data_to_eth(&frame[..len]).is_none(), "length {len}");
        }
        assert_eq!(data_to_eth(&frame).unwrap()[12..], [0x08, 0, 1, 2, 3]);
    }
}

#[test]
fn ap_configuration_keeps_defaults_and_recognizes_aliases() {
    let cfg = ApConfig::parse("").unwrap();
    assert_eq!(cfg.ssid, "esp32sim");
    assert_eq!(cfg.bssid, [2, 0x53, 0x49, 0x4d, 0, 1]);
    assert_eq!(cfg.channel, 6);
    assert!(cfg.psk.is_none());
    for channel in ["chan", "channel", "ch"] {
        for psk in ["psk", "password", "pass"] {
            let spec = format!("ssid=test,{channel}=11,{psk}=s=ecret,bssid=02:ab:CD:00:12:ff");
            let cfg = ApConfig::parse(&spec).unwrap();
            assert_eq!(cfg.ssid, "test");
            assert_eq!(cfg.channel, 11);
            assert_eq!(cfg.psk.as_deref(), Some("s=ecret"));
            assert_eq!(cfg.bssid, [2, 0xab, 0xcd, 0, 0x12, 0xff]);
        }
    }
}

fn offer(ap: &mut VirtualAp, peer: &mut StationPeer, frame: &[u8], now: u64) {
    ap.on_station_tx(frame, now);
    peer.observe(frame, now);
}

/// Guest TX is shown to both roles, the way the MAC delivers one air. The upstream
/// AP and the SoftAP peer keep separate BSSIDs and addresses, and either role can
/// drop without taking the other with it.
#[test]
fn ap_and_station_roles_move_independently() {
    let mut ap = VirtualAp::new(config(), false);
    let mut peer = StationPeer::new([0x02, 0x4e, 0x4f, 0x54, 0x45, 0x01]);
    let mut now = 1_000u64;

    offer(&mut ap, &mut peer, &station_frame(11 << 4, &[0, 0, 1, 0, 0, 0]), now);
    assert_eq!(ap.state, StaState::Authenticated);
    assert_eq!(peer.phase(), "idle", "an auth for the upstream AP is not a SoftAP beacon");
    offer(&mut ap, &mut peer, &station_frame(0, &[1, 0, 0, 0]), now);
    assert_eq!(ap.state, StaState::Associated);
    assert_ne!(ap.sta, peer.mac);
    assert_ne!(ap.cfg.bssid, [0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);

    let data = station_frame(0x0108, &[0xaa, 0xaa, 3, 0, 0, 0, 0x08, 0, 0x45]);
    let eth = ap.on_station_tx(&data, now);
    peer.observe(&data, now);
    assert!(eth.is_some(), "station data is for the upstream AP");
    assert!(peer.take_eth().is_empty());
    assert_eq!(peer.phase(), "idle");

    offer(&mut ap, &mut peer, &beacon("emini.ink", false), now);
    now += 2_000;
    let auth = peer.take_due(now);
    assert_eq!(auth.len(), 1);
    assert_eq!(auth[0].frame[0] >> 4, 11);
    offer(&mut ap, &mut peer, &mgmt_to_peer(11, &[0, 0, 2, 0, 0, 0]), now);
    now += 2_000;
    assert_eq!(peer.take_due(now)[0].frame[0] >> 4, 0);
    offer(&mut ap, &mut peer, &mgmt_to_peer(1, &[0x01, 0, 0, 0, 0x01, 0xc0]), now);
    now += 6_000;
    assert_eq!(peer.take_due(now).len(), 1, "DHCP discover");
    offer(&mut ap, &mut peer, &dhcp_offer(2), now);
    now += 2_000;
    assert!(peer.take_due(now)[0].frame.windows(3).any(|w| w == [53, 1, 3]));
    offer(&mut ap, &mut peer, &dhcp_offer(5), now);

    assert_eq!(peer.ip, Some([192, 168, 4, 2]));
    assert_eq!(peer.ssid, "emini.ink");
    assert_eq!(peer.bssid, [0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
    assert_ne!(peer.bssid, ap.cfg.bssid);
    assert_eq!(ap.state, StaState::Associated, "joining the SoftAP must not drop the station");
    assert_eq!(ap.cfg.ssid, "esp32sim");

    offer(&mut ap, &mut peer, &mgmt_to_peer(12, &[3, 0]), now);
    assert!(peer.ip.is_none(), "deauth closes the setup AP client");
    assert_eq!(peer.phase(), "idle");
    assert_eq!(ap.state, StaState::Associated, "station stays up when the setup AP closes");
    let _ = peer.take_due(now + 10_000);

    let leave = station_frame(12 << 4, &[3, 0]);
    ap.on_station_tx(&leave, now);
    peer.observe(&leave, now);
    assert_eq!(ap.state, StaState::Idle);
    assert!(peer.ip.is_none(), "the station leaving does not invent a SoftAP lease");

    offer(&mut ap, &mut peer, &beacon("emini.ink", false), now);
    now += 2_000;
    assert_eq!(peer.take_due(now).len(), 1, "the setup AP can open again");
    offer(&mut ap, &mut peer, &mgmt_to_peer(11, &[0, 0, 2, 0, 0, 0]), now);
    now += 2_000;
    let _ = peer.take_due(now);
    offer(&mut ap, &mut peer, &mgmt_to_peer(1, &[0x01, 0, 0, 0, 0x01, 0xc0]), now);
    now += 6_000;
    assert_eq!(peer.take_due(now).len(), 1, "DHCP discover after the setup AP reopens");
    offer(&mut ap, &mut peer, &dhcp_offer(2), now);
    now += 2_000;
    let _ = peer.take_due(now);
    offer(&mut ap, &mut peer, &dhcp_offer(5), now);
    assert_eq!(peer.ip, Some([192, 168, 4, 2]));
    assert_eq!(ap.state, StaState::Idle, "reopening the setup AP does not reassociate the station");
}

fn drive_wpa(ap: &mut VirtualAp, peer: &mut StationPeer, start: u64) -> bool {
    let due = ap.step(start);
    assert!(due.iter().any(|f| is_beacon(&f.frame)), "beacon at {start}");
    for frame in &due {
        if is_beacon(&frame.frame) { peer.observe(&frame.frame, start); }
    }
    let mut t = start;
    let mut echoed_rsn = false;
    for _ in 0..16 {
        t += 50_000;
        for frame in peer.take_due(t) {
            if frame.frame.windows(2).any(|w| w == [0x88, 0x8e]) && frame.frame.windows(RSN_IE.len()).any(|w| w == RSN_IE) {
                echoed_rsn = true;
            }
            ap.on_station_tx(&frame.frame, t);
        }
        for frame in ap.step(t) {
            if !is_beacon(&frame.frame) { peer.observe(&frame.frame, t); }
        }
    }
    echoed_rsn
}

#[test]
fn peer_rejoins_with_the_same_passphrase_after_release_link() {
    let mut cfg = config();
    cfg.psk = Some("esp32sim-pass".into());
    let mut ap = VirtualAp::new(cfg, false);
    let mut peer = StationPeer::new([0x02, 0x4e, 0x4f, 0x54, 0x45, 0x01]);
    peer.set_passphrase("esp32sim-pass");
    assert!(drive_wpa(&mut ap, &mut peer, 200_000));
    assert_eq!(ap.wpa.state, WpaState::Installed);
    assert_eq!(peer.supplicant, Supplicant::Installed);
    assert_eq!(peer.gtk, ap.wpa.gtk);

    peer.release_link();
    assert!(peer.has_passphrase());
    assert!(peer.ip.is_none());
    assert!(peer.ssid.is_empty());
    assert_eq!(peer.phase(), "idle");
    ap.state = StaState::Idle;
    ap.wpa.state = WpaState::Idle;
    ap.queue.clear();
    ap.next_beacon_us = 0;

    assert!(drive_wpa(&mut ap, &mut peer, 2_000_000), "message 2 still carries the RSN element");
    assert_eq!(ap.wpa.state, WpaState::Installed);
    assert_eq!(peer.supplicant, Supplicant::Installed);
    assert_eq!(peer.gtk, ap.wpa.gtk);
}

#[test]
fn ap_configuration_rejects_unknown_options_and_invalid_values() {
    for spec in [
        "passwd=secret", "pass", "ssid=x,", "=x", "channel=0", "channel=15",
        "channel=256", "channel=-1", "channel=no", "bssid=02:53:49:4d:00",
        "bssid=02:53:49:4d:00:01:02", "bssid=02:xx:53:49:4d:00:01",
        "bssid=02:53:49:4d:00:GG", "bssid=2:53:49:4d:00:01", "bssid=+2:53:49:4d:00:01",
    ] {
        assert!(ApConfig::parse(spec).is_err(), "accepted {spec}");
    }
}

fn join_open(ap: &mut VirtualAp, now: u64) {
    ap.on_station_tx(&station_frame(11 << 4, &[0, 0, 1, 0, 0, 0]), now);
    ap.on_station_tx(&station_frame(0, &[1, 0, 10, 0]), now);
}

fn ipv4_udp(src: [u8; 4], dst: [u8; 4], sport: u16, dport: u16, payload: &[u8]) -> Vec<u8> {
    let udp_len = 8 + payload.len();
    let total = 20 + udp_len;
    let mut packet = vec![0u8; total];
    packet[0] = 0x45;
    packet[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    packet[8] = 64;
    packet[9] = 17;
    packet[12..16].copy_from_slice(&src);
    packet[16..20].copy_from_slice(&dst);
    packet[20..22].copy_from_slice(&sport.to_be_bytes());
    packet[22..24].copy_from_slice(&dport.to_be_bytes());
    packet[24..26].copy_from_slice(&(udp_len as u16).to_be_bytes());
    packet[28..].copy_from_slice(payload);
    packet
}

fn ethernet(dst: [u8; 6], src: [u8; 6], ip: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(14 + ip.len());
    frame.extend_from_slice(&dst);
    frame.extend_from_slice(&src);
    frame.extend_from_slice(&[0x08, 0x00]);
    frame.extend_from_slice(ip);
    frame
}

/// 802.11 data, to the DS, carrying `eth` (Ethernet II).
fn to_ds(eth: &[u8]) -> Vec<u8> {
    let mut frame = station_frame(0x0108, &[]);
    frame[16..22].copy_from_slice(&eth[0..6]);
    frame.extend_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0]);
    frame.extend_from_slice(&eth[12..]);
    frame
}

fn dhcp(msg_type: u8) -> Vec<u8> {
    let mut body = vec![0u8; 300];
    body[0] = 1;
    body[1] = 1;
    body[2] = 6;
    body[4] = 0x11;
    body[28..34].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
    body[236..240].copy_from_slice(&[0x63, 0x82, 0x53, 0x63]);
    body[240..243].copy_from_slice(&[53, 1, msg_type]);
    body[243] = 255;
    ethernet([0xff; 6], [2, 3, 4, 5, 6, 7], &ipv4_udp([0, 0, 0, 0], [255, 255, 255, 255], 68, 67, &body))
}

fn dns_query() -> Vec<u8> {
    let query = vec![0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 1, b'a', 0, 0, 1, 0, 1];
    ethernet([2, 0x53, 0x49, 0x4d, 0, 2], [2, 3, 4, 5, 6, 7], &ipv4_udp([10, 0, 2, 15], [10, 0, 2, 3], 1234, 53, &query))
}

fn exchange(ap: &mut VirtualAp, net: &mut crate::net::VirtualNet, eth: &[u8], now: u64) -> Vec<Vec<u8>> {
    let data = ap.on_station_tx(&to_ds(eth), now).expect("associated station data");
    let back = data_to_eth(&data).expect("ethernet");
    let replies = net.handle(&back, now);
    for reply in &replies {
        assert!(ap.data_from_ds(reply).is_some(), "a reply is delivered only while associated");
    }
    replies
}

#[test]
fn station_rejoins_after_an_ap_deauth_and_renews_dhcp_and_dns() {
    let mut ap = VirtualAp::new(config(), false);
    let mut net = crate::net::VirtualNet::new(false);
    assert!(!ap.disconnect(0), "no station yet");

    join_open(&mut ap, 1_000);
    assert_eq!(ap.associations, 1);
    assert_eq!(ap.state, StaState::Associated);
    let _offer = exchange(&mut ap, &mut net, &dhcp(1), 2_000);
    assert_eq!(net.dhcp_acks, 0, "discover is offered, not acknowledged");
    exchange(&mut ap, &mut net, &dhcp(3), 3_000);
    assert_eq!(net.dhcp_acks, 1);

    assert!(ap.disconnect(4_000));
    assert_eq!(ap.state, StaState::Idle);
    assert!(ap.on_station_tx(&to_ds(&dhcp(3)), 4_000).is_none(), "the old association accepts no data");
    let due = ap.step(4_000);
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].frame[0] >> 4, 12, "deauthentication, not a log line");
    assert_eq!(&due[0].frame[4..10], &[2, 3, 4, 5, 6, 7]);
    assert_eq!(&due[0].frame[24..26], &2u16.to_le_bytes());

    // The driver has to authenticate again. A bare association is not a join.
    ap.on_station_tx(&station_frame(0, &[1, 0, 10, 0]), 5_000);
    assert_eq!(ap.associations, 1);
    assert_eq!(ap.state, StaState::Idle);
    join_open(&mut ap, 6_000);
    assert_eq!(ap.associations, 2);
    assert_eq!(ap.state, StaState::Associated);
    exchange(&mut ap, &mut net, &dhcp(1), 7_000);
    exchange(&mut ap, &mut net, &dhcp(3), 8_000);
    assert_eq!(net.dhcp_acks, 2, "the second lease is a new acknowledgement");

    let reply = exchange(&mut ap, &mut net, &dns_query(), 9_000);
    assert_eq!(net.dns_answers, 1);
    assert!(reply[0].windows(4).any(|w| w == [10, 0, 2, 3]), "the renewed address can resolve a name");
}

#[test]
fn disassociation_keeps_authentication_and_reassociation_is_its_own_response() {
    let mut ap = VirtualAp::new(config(), false);
    join_open(&mut ap, 0);
    ap.on_station_tx(&station_frame(10 << 4, &[8, 0]), 1_000);
    assert_eq!(ap.state, StaState::Authenticated, "disassociation does not require a new authentication");
    ap.queue.clear();
    ap.on_station_tx(&station_frame(2 << 4, &[1, 0, 10, 0]), 2_000);
    assert_eq!(ap.state, StaState::Associated);
    assert_eq!(ap.associations, 2);
    let due = ap.step(2_300);
    assert_eq!(due[0].frame[0] >> 4, 3, "reassociation response");
}

#[test]
fn a_second_association_restarts_the_wpa2_handshake() {
    let mut cfg = config();
    cfg.psk = Some("second-join".into());
    let mut ap = VirtualAp::new(cfg, false);
    join_open(&mut ap, 0);
    assert_eq!(ap.wpa.state, WpaState::AwaitingMessage2);
    let first_replay = ap.wpa.replay;
    ap.queue.clear();
    ap.on_station_tx(&station_frame(12 << 4, &[2, 0]), 1_000);
    assert_eq!(ap.state, StaState::Idle);
    assert_eq!(ap.wpa.state, WpaState::Idle);
    join_open(&mut ap, 2_000);
    assert_eq!(ap.associations, 2);
    assert_eq!(ap.wpa.state, WpaState::AwaitingMessage2);
    assert!(ap.wpa.replay > first_replay, "the new message 1 carries a higher replay counter");
    assert!(ap.queue.iter().any(|a| a.frame.windows(2).any(|w| w == [0x88, 0x8e])));
}
