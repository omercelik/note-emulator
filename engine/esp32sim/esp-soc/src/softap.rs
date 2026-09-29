//! Relay between a browser on the Mac and a SoftAP the guest is running.
//!
//! The emulated station ([`crate::wifi::StationPeer`]) joins that AP and takes a DHCP
//! lease. This relay answers ARP for the leased address and carries inbound TCP as
//! that station: the guest AP sees a client on 192.168.4.0, not the 10.0.2.0 NAT.
//! Outbound from the guest is left to the station path.

use std::io;
use std::net::SocketAddr;

use crate::nat::Nat;
use crate::net::packet::ethernet;

pub struct SoftApRelay {
    nat: Nat,
    mac: [u8; 6],
    ip: [u8; 4],
    ap_mac: [u8; 6],
    ap_ip: [u8; 4],
}

impl SoftApRelay {
    pub fn new(mac: [u8; 6]) -> Self {
        let mut nat = Nat::new(false);
        nat.outbound = false;
        Self { nat, mac, ip: [0; 4], ap_mac: [0; 6], ap_ip: [192, 168, 4, 1] }
    }

    /// Bind `bind` and carry each accepted connection to the AP's `guest_port`.
    /// The guest addresses are filled in by [`Self::set_lease`] before any accept.
    pub fn listen(&mut self, bind: SocketAddr, guest_port: u16) -> io::Result<SocketAddr> {
        self.nat.listen(bind, self.ap_mac, self.ap_ip, guest_port, self.ip)
    }

    pub fn adopt(&mut self, listener: std::net::TcpListener, guest_port: u16) -> io::Result<SocketAddr> {
        self.nat.adopt_listener(listener, self.ap_mac, self.ap_ip, guest_port, self.ip)
    }

    /// The station finished DHCP. Later accepts use this address; the relay
    /// stays quiet until then so a browser does not SYN from 0.0.0.0.
    pub fn set_lease(&mut self, ip: [u8; 4], ap_mac: [u8; 6], ap_ip: [u8; 4]) {
        self.ip = ip;
        self.ap_mac = ap_mac;
        if ap_ip != [0; 4] { self.ap_ip = ap_ip; }
        self.nat.set_inbound_target(self.ap_mac, self.ap_ip, ip);
    }

    pub fn leased(&self) -> bool { self.ip != [0; 4] }

    /// One Ethernet frame from the guest AP. Returns frames to inject back.
    pub fn handle(&mut self, eth: &[u8], now_us: u64) -> Vec<Vec<u8>> {
        if eth.len() < 14 || !self.leased() { return Vec::new(); }
        let mut src = [0u8; 6];
        src.copy_from_slice(&eth[6..12]);
        match u16::from_be_bytes([eth[12], eth[13]]) {
            0x0806 => self.arp(&eth[14..], &src),
            0x0800 => self.ipv4(&eth[14..], &src, now_us),
            _ => Vec::new(),
        }
    }

    pub fn poll(&mut self, now_us: u64) -> Vec<Vec<u8>> {
        if !self.leased() { return Vec::new(); }
        self.nat.poll(now_us)
    }

    /// Snapshot restore hook. Closes this relay's host sockets; the handshake is unchanged.
    pub fn on_snapshot_restore(&mut self) -> Vec<Vec<u8>> {
        crate::wifi::close_host_flows_for_restore(&mut self.nat)
    }

    pub fn host_socket_count(&self) -> usize { self.nat.host_socket_count() }

    fn arp(&mut self, payload: &[u8], src: &[u8; 6]) -> Vec<Vec<u8>> {
        if payload.len() < 28 || u16::from_be_bytes([payload[6], payload[7]]) != 1 { return Vec::new(); }
        if payload[24..28] != self.ip { return Vec::new(); }
        let mut body = Vec::with_capacity(28);
        body.extend_from_slice(&[0, 1, 8, 0, 6, 4, 0, 2]);
        body.extend_from_slice(&self.mac);
        body.extend_from_slice(&self.ip);
        body.extend_from_slice(src);
        body.extend_from_slice(&payload[14..18]);
        vec![ethernet(src, &self.mac, 0x0806, &body)]
    }

    fn ipv4(&mut self, packet: &[u8], src: &[u8; 6], now_us: u64) -> Vec<Vec<u8>> {
        if packet.len() < 20 || packet[0] >> 4 != 4 { return Vec::new(); }
        let ihl = ((packet[0] & 0xf) as usize) * 4;
        if ihl < 20 || packet.len() < ihl { return Vec::new(); }
        let total = u16::from_be_bytes([packet[2], packet[3]]) as usize;
        if total < ihl || total > packet.len() || u16::from_be_bytes([packet[6], packet[7]]) & 0x3fff != 0 {
            return Vec::new();
        }
        if packet[9] != 6 || packet[16..20] != self.ip { return Vec::new(); }
        let mut sip = [0u8; 4];
        sip.copy_from_slice(&packet[12..16]);
        self.nat.tcp_in(src, &sip, &self.ip, &packet[ihl..total], now_us)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpStream};
    use std::time::Duration;

    const PEER: [u8; 6] = [0x02, 0x4e, 0x4f, 0x54, 0x45, 0x01];
    const AP: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
    const PEER_IP: [u8; 4] = [192, 168, 4, 2];
    const AP_IP: [u8; 4] = [192, 168, 4, 1];

    fn arp_who_has() -> Vec<u8> {
        let mut body = vec![0, 1, 8, 0, 6, 4, 0, 1];
        body.extend_from_slice(&AP);
        body.extend_from_slice(&AP_IP);
        body.extend_from_slice(&[0; 6]);
        body.extend_from_slice(&PEER_IP);
        ethernet(&[0xff; 6], &AP, 0x0806, &body)
    }

    #[test]
    fn arp_for_the_lease_is_answered_with_the_station_mac() {
        let mut relay = SoftApRelay::new(PEER);
        assert!(relay.handle(&arp_who_has(), 0).is_empty(), "no lease yet");
        relay.set_lease(PEER_IP, AP, AP_IP);
        let reply = relay.handle(&arp_who_has(), 0);
        assert_eq!(reply.len(), 1);
        assert_eq!(&reply[0][0..6], &AP);
        assert_eq!(&reply[0][6..12], &PEER);
        assert_eq!(&reply[0][12..14], &[0x08, 0x06]);
        assert_eq!(&reply[0][22..28], &PEER);
        assert_eq!(&reply[0][28..32], &PEER_IP);
    }

    #[test]
    fn a_browser_syn_is_a_station_on_the_softap_and_the_reply_comes_back() {
        let mut relay = SoftApRelay::new(PEER);
        let bound = relay.listen((Ipv4Addr::LOCALHOST, 0).into(), 80).unwrap();
        relay.set_lease(PEER_IP, AP, AP_IP);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(TcpStream::connect(bound).unwrap()).unwrap());
        let mut syn = Vec::new();
        for i in 0..50 {
            syn = relay.poll((i as u64) * 20_000);
            if !syn.is_empty() { break; }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(syn.len(), 1, "accept injects one SYN");
        assert_eq!(&syn[0][0..6], &AP, "destination is the AP");
        assert_eq!(&syn[0][26..30], &PEER_IP);
        assert_eq!(&syn[0][30..34], &AP_IP);
        let tcp = &syn[0][34..];
        assert_eq!(tcp[13], 0x02);
        assert_eq!(u16::from_be_bytes([tcp[2], tcp[3]]), 80);
        let sport = u16::from_be_bytes([tcp[0], tcp[1]]);
        let isn = u32::from_be_bytes(tcp[4..8].try_into().unwrap());

        let mut client = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        client.set_nodelay(true).unwrap();

        let mut seg = vec![0u8; 20];
        seg[0..2].copy_from_slice(&80u16.to_be_bytes());
        seg[2..4].copy_from_slice(&sport.to_be_bytes());
        seg[4..8].copy_from_slice(&1000u32.to_be_bytes());
        seg[8..12].copy_from_slice(&isn.wrapping_add(1).to_be_bytes());
        seg[12] = 0x50;
        seg[13] = 0x12;
        seg[14..16].copy_from_slice(&5840u16.to_be_bytes());
        let mut ip = crate::net::packet::ip_packet(6, &AP_IP, &PEER_IP, &seg);
        // The relay does not check the TCP checksum; the NAT fills its own on the way out.
        let frame = ethernet(&PEER, &AP, 0x0800, &ip);
        let ack = relay.handle(&frame, 1_000_000);
        assert_eq!(ack.len(), 1);
        assert_eq!(ack[0][34 + 13], 0x10, "handshake ACK");

        client.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
        let mut to_ap = Vec::new();
        for i in 0..50 {
            to_ap = relay.poll(2_000_000 + (i as u64) * 20_000);
            if !to_ap.is_empty() { break; }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(to_ap[0].windows(18).any(|w| w == b"GET / HTTP/1.0\r\n\r\n"));

        let response = b"HTTP/1.0 200 OK\r\n\r\nhi";
        let mut seg = vec![0u8; 20];
        seg[0..2].copy_from_slice(&80u16.to_be_bytes());
        seg[2..4].copy_from_slice(&sport.to_be_bytes());
        seg[4..8].copy_from_slice(&1001u32.to_be_bytes());
        let req_end = u32::from_be_bytes(to_ap[0][34 + 4..34 + 8].try_into().unwrap()).wrapping_add(18);
        seg[8..12].copy_from_slice(&req_end.to_be_bytes());
        seg[12] = 0x50;
        seg[13] = 0x19; // PSH, ACK, FIN so the browser sees the end of the response
        seg[14..16].copy_from_slice(&5840u16.to_be_bytes());
        seg.extend_from_slice(response);
        ip = crate::net::packet::ip_packet(6, &AP_IP, &PEER_IP, &seg);
        let _ = relay.handle(&ethernet(&PEER, &AP, 0x0800, &ip), 3_000_000);
        relay.poll(3_000_000);
        let mut got = Vec::new();
        client.read_to_end(&mut got).unwrap();
        assert_eq!(got, response);
    }
}
