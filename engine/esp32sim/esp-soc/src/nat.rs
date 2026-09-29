//! User-mode NAT: guest TCP and UDP flows are relayed through ordinary host sockets.
//!
//! Outbound (guest is the TCP client) dials a host socket and speaks the server side of the
//! virtual connection. Inbound (a program on the Mac is the client) accepts on a listener the
//! runtime owns and injects a SYN toward the guest server. Both directions share one flow table,
//! bounded buffers and one retransmission queue, so a dropped virtual-air frame cannot discard
//! acknowledged bytes or strand a handshake. No window scaling, SACK or congestion control.

use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use crate::net::packet::{ethernet, ip_packet, transport_checksum, udp_packet, GATEWAY_MAC};

const MSS: usize = 1400;
const WINDOW: u16 = 5840;
const RETRANSMIT_US: u64 = 300_000;
const IDLE_CLOSE_US: u64 = 120_000_000;
const TIME_WAIT_US: u64 = 30_000_000;
const MAX_FLOWS: usize = 64;
const MAX_CONNECTS: usize = 8;
const UDP_BATCH: usize = 16;
const FIN: u8 = 0x01;
const SYN: u8 = 0x02;
const RST: u8 = 0x04;
const ACK: u8 = 0x10;
const PSH: u8 = 0x08;

fn ip(a: &[u8; 4]) -> Ipv4Addr { Ipv4Addr::from(*a) }

/// Close a connected socket with a RST. A plain drop completes the handshake with FIN,
/// which a peer waiting on this connection would treat as a clean end of data.
fn abort_socket(transport: &mut Transport) {
    if let Transport::Connected(sock) = transport {
        let linger = libc::linger { l_onoff: 1, l_linger: 0 };
        unsafe {
            libc::setsockopt(sock.as_raw_fd(), libc::SOL_SOCKET, libc::SO_LINGER,
                &linger as *const libc::linger as *const libc::c_void,
                std::mem::size_of::<libc::linger>() as libc::socklen_t);
        }
    }
    *transport = Transport::Closed;
}
fn address(a: &[u8; 4], port: u16) -> SocketAddr { SocketAddr::new(IpAddr::V4(ip(a)), port) }

// A permit belongs to the blocking connect worker, not its guest flow. Removing a flow with RST
// must not free a slot while connect_timeout is still running on the host.
static CONNECTING: AtomicUsize = AtomicUsize::new(0);
struct ConnectPermit<'a>(&'a AtomicUsize);
impl<'a> ConnectPermit<'a> {
    fn acquire(counter: &'a AtomicUsize) -> Option<Self> {
        counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| (n < MAX_CONNECTS).then_some(n + 1)).ok().map(|_| Self(counter))
    }
}
impl Drop for ConnectPermit<'_> {
    fn drop(&mut self) { self.0.fetch_sub(1, Ordering::Relaxed); }
}

fn connect(addr: SocketAddr) -> Option<Receiver<io::Result<TcpStream>>> {
    let permit = ConnectPermit::acquire(&CONNECTING)?;
    let (tx, rx) = channel();
    std::thread::Builder::new().name("nat-connect".into()).spawn(move || {
        let _permit = permit;
        let result = TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(4))
            .and_then(|sock| { sock.set_nonblocking(true)?; sock.set_nodelay(true)?; Ok(sock) });
        let _ = tx.send(result);
    }).ok()?;
    Some(rx)
}

enum Transport { Connecting(Receiver<io::Result<TcpStream>>), Connected(TcpStream), TimeWait, Closed }
#[derive(Clone, Copy, PartialEq, Eq)]
enum GuestWrite { Open, Draining, Closed }
/// `Server`: the guest opened the connection (we dial the host). `Client`: the host opened it
/// (we inject the SYN). The struct's port fields keep their names either way — `guest_port` is
/// the guest's port, `dst_port` is the port on our side of the virtual TCP.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role { Server, Client }

struct Sent {
    seq: u32,
    flags: u8,
    data: Vec<u8>,
    at_us: u64,
}
impl Sent {
    fn len(&self) -> usize { self.data.len() + usize::from(self.flags & (SYN | FIN) != 0) }
}

struct Tcp {
    role: Role,
    /// Which inbound listener created this flow. `None` for guest-initiated connections.
    listener: Option<u64>,
    guest_mac: [u8; 6], guest_ip: [u8; 4], guest_port: u16, dst_ip: [u8; 4], dst_port: u16,
    transport: Transport,
    guest_write: GuestWrite,
    host_closed: bool,
    /// Inbound only: set once the guest's SYN-ACK has been accepted. Stays false while our SYN
    /// is outstanding, including when a bare ACK would otherwise retire it.
    linked: bool,
    our_seq: u32,
    guest_seq: u32,
    guest_window: u16,
    to_host: VecDeque<u8>,
    unacked: VecDeque<Sent>,
    last_activity_us: u64,
    /// Sequence number last resent on a duplicate ACK, so one loss is fast-retransmitted once.
    fast_resent: Option<u32>,
}

impl Tcp {
    fn handshake_pending(&self) -> bool { self.unacked.front().is_some_and(|s| s.flags & SYN != 0) }
    fn in_flight(&self) -> usize { self.unacked.iter().map(Sent::len).sum() }

    fn acknowledge(&mut self, ack: u32) {
        let Some(first) = self.unacked.front() else { return };
        let mut count = ack.wrapping_sub(first.seq) as usize;
        if count > self.in_flight() { return; } // old or beyond anything sent
        while count > 0 {
            let sent = self.unacked.front_mut().unwrap();
            let n = sent.len();
            if count < n {
                sent.data.drain(..count);
                sent.seq = ack;
                break;
            }
            count -= n;
            self.unacked.pop_front();
        }
    }

    fn accept(&mut self, seq: u32, data: &[u8], fin: bool) {
        if self.guest_write != GuestWrite::Open || seq != self.guest_seq { return; }
        if data.len() > WINDOW as usize - self.to_host.len() { return; }
        self.to_host.extend(data);
        self.guest_seq = self.guest_seq.wrapping_add(data.len() as u32);
        if fin {
            self.guest_seq = self.guest_seq.wrapping_add(1);
            self.guest_write = GuestWrite::Draining;
        }
    }

    fn segment(&self, flags: u8, data: &[u8], seq: u32) -> Vec<u8> {
        let mut seg = Vec::with_capacity(20 + data.len());
        seg.extend_from_slice(&self.dst_port.to_be_bytes());
        seg.extend_from_slice(&self.guest_port.to_be_bytes());
        seg.extend_from_slice(&seq.to_be_bytes());
        seg.extend_from_slice(&self.guest_seq.to_be_bytes());
        seg.extend_from_slice(&[0x50, flags]);
        seg.extend_from_slice(&(WINDOW - self.to_host.len() as u16).to_be_bytes());
        seg.extend_from_slice(&[0; 4]);
        seg.extend_from_slice(data);
        let check = transport_checksum(&self.dst_ip, &self.guest_ip, 6, &seg);
        seg[16..18].copy_from_slice(&check.to_be_bytes());
        ethernet(&self.guest_mac, &GATEWAY_MAC, 0x0800, &ip_packet(6, &self.dst_ip, &self.guest_ip, &seg))
    }

    fn send(&mut self, flags: u8, data: Vec<u8>, now_us: u64) -> Vec<u8> {
        let frame = self.segment(flags, &data, self.our_seq);
        let sent = Sent { seq: self.our_seq, flags, data, at_us: now_us };
        self.our_seq = self.our_seq.wrapping_add(sent.len() as u32);
        self.unacked.push_back(sent);
        frame
    }

    fn retransmit(&mut self, now_us: u64) -> Option<Vec<u8>> {
        let sent = self.unacked.front()?;
        if now_us.wrapping_sub(sent.at_us) < RETRANSMIT_US { return None; }
        self.resend_front(now_us)
    }

    /// Send the oldest unacknowledged segment again and restart its timer.
    fn resend_front(&mut self, now_us: u64) -> Option<Vec<u8>> {
        let sent = self.unacked.front_mut()?;
        sent.at_us = now_us;
        let sent = self.unacked.front().unwrap();
        Some(self.segment(sent.flags, &sent.data, sent.seq))
    }

    fn closed(&self) -> bool {
        matches!(self.transport, Transport::Closed)
    }
}

/// Preserve every unwritten byte across short writes and WouldBlock. Returns bytes actually sent.
fn flush_pending(writer: &mut impl Write, pending: &mut VecDeque<u8>) -> io::Result<usize> {
    let mut written = 0;
    while !pending.is_empty() {
        match writer.write(pending.as_slices().0) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(n) => { pending.drain(..n); written += n; }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) => return Err(e),
        }
    }
    Ok(written)
}

struct Udp {
    guest_mac: [u8; 6], guest_ip: [u8; 4], guest_port: u16, dst_ip: [u8; 4], dst_port: u16,
    reply_src: [u8; 4], // DNS's guest-visible address, before host resolver redirection
    sock: UdpSocket,
    last_activity_us: u64,
}

/// A host listener whose accepted sockets become client-role flows toward one guest port.
struct Inbound {
    id: u64,
    sock: TcpListener,
    guest_mac: [u8; 6],
    guest_ip: [u8; 4],
    guest_port: u16,
    local_ip: [u8; 4],
}

pub struct Nat {
    tcp: Vec<Tcp>,
    udp: Vec<Udp>,
    listeners: Vec<Inbound>,
    isn: u32,
    next_port: u16,
    next_listener: u64,
    /// Guest-initiated TCP/UDP reaches the host only when set. Inbound listeners work either way,
    /// so a setup-address forward does not by itself open outbound access (ADR-014).
    pub outbound: bool,
    pub resolver: [u8; 4],
    pub log: bool,
    pub tcp_opened: u64, pub tcp_refused: u64, pub inbound_opened: u64,
    pub udp_flows: u64,
    pub udp_evicted: u64, pub udp_send_errors: u64,
    pub bytes_to_host: u64, pub bytes_to_guest: u64,
}

impl Nat {
    pub fn new(log: bool) -> Self {
        Self { tcp: Vec::new(), udp: Vec::new(), listeners: Vec::new(), isn: 0x1000, next_port: 49152, next_listener: 1,
            outbound: true, resolver: host_resolver(), log,
            tcp_opened: 0, tcp_refused: 0, inbound_opened: 0, udp_flows: 0, udp_evicted: 0, udp_send_errors: 0,
            bytes_to_host: 0, bytes_to_guest: 0 }
    }

    /// Bind `bind` and forward each accepted connection to `guest_ip:guest_port`.
    /// The guest sees the connection as coming from `local_ip` (the virtual gateway).
    pub fn listen(&mut self, bind: SocketAddr, guest_mac: [u8; 6], guest_ip: [u8; 4], guest_port: u16, local_ip: [u8; 4]) -> io::Result<SocketAddr> {
        self.adopt_listener(TcpListener::bind(bind)?, guest_mac, guest_ip, guest_port, local_ip)
    }

    /// Take a listener that was bound elsewhere (the setup-address fd the helper passes in).
    pub fn adopt_listener(&mut self, sock: TcpListener, guest_mac: [u8; 6], guest_ip: [u8; 4], guest_port: u16, local_ip: [u8; 4]) -> io::Result<SocketAddr> {
        sock.set_nonblocking(true)?;
        let addr = sock.local_addr()?;
        if self.log { eprintln!("[nat] listening on {addr} -> guest {}:{}", ip(&guest_ip), guest_port); }
        self.next_listener += 1;
        self.listeners.push(Inbound { id: self.next_listener, sock, guest_mac, guest_ip, guest_port, local_ip });
        Ok(addr)
    }

    /// Point later accepts at the SoftAP the station just joined. Flows already
    /// accepted keep the addresses they were opened with.
    pub fn set_inbound_target(&mut self, guest_mac: [u8; 6], guest_ip: [u8; 4], local_ip: [u8; 4]) {
        for listener in &mut self.listeners {
            listener.guest_mac = guest_mac;
            listener.guest_ip = guest_ip;
            listener.local_ip = local_ip;
        }
    }

    /// Drop one listener, identified by the address `listen`/`adopt_listener` returned,
    /// and RST only the flows it accepted.
    pub fn close_bound(&mut self, addr: SocketAddr) -> Vec<Vec<u8>> {
        let Some(i) = self.listeners.iter().position(|l| l.sock.local_addr().ok() == Some(addr)) else { return Vec::new() };
        let id = self.listeners[i].id;
        self.listeners.remove(i);
        self.reset_listener(id)
    }

    fn reset_listener(&mut self, id: u64) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for flow in &mut self.tcp {
            if flow.listener == Some(id) && !matches!(flow.transport, Transport::Closed | Transport::TimeWait) {
                out.push(flow.segment(RST | ACK, &[], flow.our_seq));
                abort_socket(&mut flow.transport);
            }
        }
        out
    }

    /// How many host sockets this NAT still owns (listeners, TCP flows, UDP flows).
    pub fn host_socket_count(&self) -> usize { self.listeners.len() + self.tcp.len() + self.udp.len() }

    /// Drop every host socket. Live TCP is not snapshot state: callers must not keep the
    /// returned RST frames attached to a restored guest sequence space. Connecting workers
    /// release their own permits when they finish; this does not touch `CONNECTING`.
    pub fn close_host_flows(&mut self) -> Vec<Vec<u8>> {
        self.listeners.clear();
        let mut out = Vec::new();
        for flow in &mut self.tcp {
            if !matches!(flow.transport, Transport::Closed | Transport::TimeWait) {
                out.push(flow.segment(RST | ACK, &[], flow.our_seq));
                abort_socket(&mut flow.transport);
            }
        }
        self.tcp.clear();
        self.udp.clear();
        out
    }

    /// Drop every inbound listener and RST its flows. The caller injects the returned frames.
    pub fn close_inbound(&mut self) -> Vec<Vec<u8>> {
        self.listeners.clear();
        let mut out = Vec::new();
        for flow in &mut self.tcp {
            if flow.role == Role::Client && !matches!(flow.transport, Transport::Closed | Transport::TimeWait) {
                out.push(flow.segment(RST | ACK, &[], flow.our_seq));
                abort_socket(&mut flow.transport);
            }
        }
        out
    }

    fn room_for_flow(&mut self) -> bool {
        if self.tcp.len() < MAX_FLOWS { return true; }
        let Some(i) = self.tcp.iter().position(|c| matches!(c.transport, Transport::TimeWait | Transport::Closed)) else { return false };
        self.tcp.remove(i);
        true
    }

    fn alloc_ephemeral(&mut self) -> Option<u16> {
        for _ in 0..128 {
            let candidate = self.next_port;
            self.next_port = if self.next_port == 65535 { 49152 } else { self.next_port + 1 };
            let used = self.tcp.iter().any(|c| c.role == Role::Client && c.dst_port == candidate && !c.closed());
            if !used { return Some(candidate); }
        }
        None
    }

    /// Non-blocking accept. A full live table resets the one connection just taken off the
    /// backlog and leaves the rest queued.
    fn accept_inbound(&mut self, now_us: u64) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for li in 0..self.listeners.len() {
            loop {
                let accepted = match self.listeners[li].sock.accept() {
                    Ok((stream, _)) => stream,
                    Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => break,
                    // The peer gave up while queued (ECONNABORTED): skip it, keep accepting.
                    Err(e) if e.kind() == ErrorKind::ConnectionAborted => continue,
                    Err(e) => {
                        if self.log { eprintln!("[nat] inbound accept: {e}"); }
                        break;
                    }
                };
                // Evict only once a host connection is actually waiting. Checking first would
                // discard a finished flow on every idle poll.
                if !self.room_for_flow() { break; }
                if accepted.set_nonblocking(true).is_err() || accepted.set_nodelay(true).is_err() { continue; }
                let Some(port) = self.alloc_ephemeral() else { break; };
                let listener = &self.listeners[li];
                let listener_id = listener.id;
                self.isn = self.isn.wrapping_add(0x10000);
                let mut flow = Tcp {
                    role: Role::Client, listener: Some(listener_id),
                    guest_mac: listener.guest_mac, guest_ip: listener.guest_ip,
                    guest_port: listener.guest_port, dst_ip: listener.local_ip, dst_port: port,
                    transport: Transport::Connected(accepted), guest_write: GuestWrite::Open, host_closed: false,
                    linked: false, our_seq: self.isn, guest_seq: 0, guest_window: WINDOW,
                    to_host: VecDeque::new(), unacked: VecDeque::new(), last_activity_us: now_us, fast_resent: None,
                };
                let syn = flow.send(SYN, Vec::new(), now_us);
                if self.log {
                    eprintln!("[nat] inbound {}:{} -> guest {}:{} SYN", ip(&flow.dst_ip), flow.dst_port, ip(&flow.guest_ip), flow.guest_port);
                }
                self.tcp.push(flow);
                self.inbound_opened += 1;
                out.push(syn);
            }
        }
        out
    }

    /// Forward a UDP datagram through a connected socket, which accepts replies only from its peer.
    #[allow(clippy::too_many_arguments, reason = "packet fields stay explicit at the protocol boundary")]
    pub fn udp_out(&mut self, gmac: &[u8; 6], gip: &[u8; 4], sport: u16, dip: &[u8; 4], reply_src: &[u8; 4],
                   dport: u16, payload: &[u8], now_us: u64) {
        let idx = self.udp.iter().position(|f| f.guest_ip == *gip && f.guest_port == sport && f.dst_ip == *dip && f.dst_port == dport);
        let idx = match idx {
            Some(i) => i,
            None => {
                let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else { return };
                if sock.connect(address(dip, dport)).is_err() || sock.set_nonblocking(true).is_err() { return; }
                if self.udp.len() >= MAX_FLOWS {
                    let oldest = self.udp.iter().enumerate().max_by_key(|(_, f)| now_us.wrapping_sub(f.last_activity_us)).unwrap().0;
                    self.udp.remove(oldest);
                    self.udp_evicted += 1;
                    if self.log { eprintln!("[nat] UDP evicted least recently active flow (table full)"); }
                }
                self.udp.push(Udp { guest_mac: *gmac, guest_ip: *gip, guest_port: sport, dst_ip: *dip, dst_port: dport,
                    reply_src: *reply_src, sock, last_activity_us: now_us });
                self.udp_flows += 1;
                if self.log { eprintln!("[nat] UDP {}:{} -> {}:{} ({} bytes)", ip(gip), sport, ip(dip), dport, payload.len()); }
                self.udp.len() - 1
            }
        };
        let flow = &mut self.udp[idx];
        flow.last_activity_us = now_us;
        match flow.sock.send(payload) {
            Ok(n) => self.bytes_to_host += n as u64,
            Err(e) => {
                self.udp_send_errors += 1;
                if self.log { eprintln!("[nat] UDP {}:{} send failed: {}", ip(dip), dport, e); }
            }
        }
    }

    pub fn tcp_in(&mut self, gmac: &[u8; 6], gip: &[u8; 4], dip: &[u8; 4], seg: &[u8], now_us: u64) -> Vec<Vec<u8>> {
        self.tcp_in_with_connect(gmac, gip, dip, seg, now_us, connect)
    }

    #[allow(clippy::too_many_arguments, reason = "inject the connector without process-global state in admission tests")]
    fn tcp_in_with_connect(&mut self, gmac: &[u8; 6], gip: &[u8; 4], dip: &[u8; 4], seg: &[u8], now_us: u64,
                           connector: impl FnOnce(SocketAddr) -> Option<Receiver<io::Result<TcpStream>>>) -> Vec<Vec<u8>> {
        if seg.len() < 20 { return Vec::new(); }
        let off = ((seg[12] >> 4) as usize) * 4;
        if off < 20 || off > seg.len() { return Vec::new(); }
        let sport = u16::from_be_bytes([seg[0], seg[1]]);
        let dport = u16::from_be_bytes([seg[2], seg[3]]);
        let seq = u32::from_be_bytes(seg[4..8].try_into().unwrap());
        let ack = u32::from_be_bytes(seg[8..12].try_into().unwrap());
        let flags = seg[13];
        let data = &seg[off..];
        let window = u16::from_be_bytes([seg[14], seg[15]]);
        let idx = self.tcp.iter().position(|c| c.guest_ip == *gip && c.guest_port == sport && c.dst_port == dport && c.dst_ip == *dip);
        if flags & (SYN | ACK | RST) == SYN && idx.is_none() {
            if !self.outbound { return Vec::new(); }
            let evict = if self.tcp.len() >= MAX_FLOWS {
                // Finished flows remember final ACKs only while their bounded slots are spare.
                let Some(i) = self.tcp.iter().position(|c| matches!(c.transport, Transport::TimeWait | Transport::Closed)) else { return Vec::new() };
                Some(i)
            } else { None };
            let Some(pending) = connector(address(dip, dport)) else { return Vec::new() };
            if let Some(i) = evict { self.tcp.remove(i); }
            self.isn = self.isn.wrapping_add(0x10000);
            self.tcp.push(Tcp { role: Role::Server, listener: None, guest_mac: *gmac, guest_ip: *gip, guest_port: sport, dst_ip: *dip, dst_port: dport,
                transport: Transport::Connecting(pending), guest_write: GuestWrite::Open, host_closed: false, linked: false,
                our_seq: self.isn, guest_seq: seq.wrapping_add(1), guest_window: window,
                to_host: VecDeque::new(), unacked: VecDeque::new(), last_activity_us: now_us, fast_resent: None });
            if self.log { eprintln!("[nat] TCP {}:{} -> {}:{} connecting", ip(gip), sport, ip(dip), dport); }
            return Vec::new();
        }
        let Some(i) = idx else { return Vec::new() };
        let c = &mut self.tcp[i];
        c.last_activity_us = now_us;
        if flags & RST != 0 { abort_socket(&mut c.transport); return Vec::new(); }
        if matches!(c.transport, Transport::TimeWait) {
            return if flags & FIN != 0 { vec![c.segment(ACK, &[], c.our_seq)] } else { Vec::new() };
        }
        if flags & SYN != 0 {
            if c.role == Role::Client {
                // The guest is the server: a SYN-ACK links the flow, a bare SYN is not a handshake.
                if flags & ACK == 0 { return Vec::new(); }
                if c.handshake_pending() {
                    c.acknowledge(ack);
                    if c.handshake_pending() { return Vec::new(); }
                    c.guest_seq = seq.wrapping_add(1);
                    c.guest_window = window;
                    c.linked = true;
                }
                return vec![c.segment(ACK, &[], c.our_seq)];
            }
            return if c.handshake_pending() { vec![c.segment(SYN | ACK, &[], c.unacked[0].seq)] } else { Vec::new() };
        }
        if c.role == Role::Client && !c.linked { return Vec::new(); }
        if !matches!(c.transport, Transport::Connected(_)) { return Vec::new(); }
        let mut resend = None;
        if flags & ACK != 0 {
            // A duplicate ACK (no data, same window, our data still outstanding) means the guest
            // has a hole. The emulated air never reorders, so resend the hole at once instead of
            // stalling for the retransmission timeout (fast retransmit).
            let duplicate = data.is_empty() && flags & FIN == 0 && window == c.guest_window
                && c.unacked.front().is_some_and(|f| f.seq == ack && f.flags & SYN == 0);
            c.acknowledge(ack);
            c.guest_window = window;
            if duplicate && c.fast_resent != Some(ack) {
                c.fast_resent = Some(ack);
                resend = c.resend_front(now_us);
            }
        }
        if c.handshake_pending() { return Vec::new(); }
        if let Some(frame) = resend { return vec![frame]; }
        if !data.is_empty() || flags & FIN != 0 {
            c.accept(seq, data, flags & FIN != 0);
            return vec![c.segment(ACK, &[], c.our_seq)];
        }
        Vec::new()
    }

    /// Pump a bounded amount of host traffic, then retransmit and expire flows.
    pub fn poll(&mut self, now_us: u64) -> Vec<Vec<u8>> {
        let mut out = self.accept_inbound(now_us);
        for c in &mut self.tcp {
            if c.closed() { continue; }
            if c.host_closed && c.guest_write == GuestWrite::Closed && c.unacked.is_empty()
                && matches!(c.transport, Transport::Connected(_)) {
                // Release the host socket but keep enough state to re-ACK a lost final ACK.
                c.transport = Transport::TimeWait;
                c.last_activity_us = now_us;
            }
            if let Transport::Connecting(rx) = &c.transport {
                let result = match rx.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Disconnected) => Some(Err(ErrorKind::ConnectionAborted.into())),
                    Err(TryRecvError::Empty) => None,
                };
                match result {
                    Some(Ok(sock)) => {
                        c.transport = Transport::Connected(sock);
                        self.tcp_opened += 1;
                        if self.log { eprintln!("[nat] TCP {}:{} connected", ip(&c.dst_ip), c.dst_port); }
                        out.push(c.send(SYN | ACK, Vec::new(), now_us));
                    }
                    Some(Err(e)) => {
                        if self.log { eprintln!("[nat] TCP {}:{} failed: {}", ip(&c.dst_ip), c.dst_port, e); }
                        self.tcp_refused += 1;
                        out.push(c.segment(RST | ACK, &[], c.our_seq));
                        c.transport = Transport::Closed;
                    }
                    None => {}
                }
            }
            let peer_ready = if c.role == Role::Client { c.linked } else { !c.handshake_pending() };
            if matches!(c.transport, Transport::Connected(_)) && peer_ready {
                let Transport::Connected(sock) = &mut c.transport else { unreachable!() };
                let queued = c.to_host.len();
                match flush_pending(sock, &mut c.to_host) {
                    Ok(n) => self.bytes_to_host += n as u64,
                    Err(_) => { out.push(c.segment(RST | ACK, &[], c.our_seq)); c.transport = Transport::Closed; continue; }
                }
                if c.guest_write == GuestWrite::Draining && c.to_host.is_empty() {
                    let _ = sock.shutdown(std::net::Shutdown::Write);
                    c.guest_write = GuestWrite::Closed;
                }
                if c.to_host.len() < queued { out.push(c.segment(ACK, &[], c.our_seq)); } // reopen the receive window
                // One byte at a zero window becomes a persist probe. The same retransmission
                // queue retries it, recovering even when the guest's window-update ACK is lost.
                let send_window = usize::from(c.guest_window.clamp(1, WINDOW));
                while !c.host_closed && c.in_flight() < send_window {
                    let room = send_window - c.in_flight();
                    let mut buf = [0; MSS];
                    let Transport::Connected(sock) = &mut c.transport else { break };
                    match sock.read(&mut buf[..room.min(MSS)]) {
                        Ok(0) => { c.host_closed = true; out.push(c.send(FIN | ACK, Vec::new(), now_us)); }
                        Ok(n) => {
                            if self.log { eprintln!("[nat] TCP {}:{} -> guest {} bytes (seq {})", ip(&c.dst_ip), c.dst_port, n, c.our_seq); }
                            self.bytes_to_guest += n as u64;
                            c.last_activity_us = now_us;
                            out.push(c.send(PSH | ACK, buf[..n].to_vec(), now_us));
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(_) => { out.push(c.segment(RST | ACK, &[], c.our_seq)); c.transport = Transport::Closed; break; }
                    }
                }
            }
            if let Some(frame) = c.retransmit(now_us) { out.push(frame); }
        }
        for flow in &mut self.udp {
            let mut buf = [0; 2048];
            for _ in 0..UDP_BATCH {
                let Ok(n) = flow.sock.recv(&mut buf) else { break };
                if self.log { eprintln!("[nat] UDP reply {} bytes -> guest port {}", n, flow.guest_port); }
                self.bytes_to_guest += n as u64;
                flow.last_activity_us = now_us;
                let udp = udp_packet(&flow.reply_src, &flow.guest_ip, flow.dst_port, flow.guest_port, &buf[..n]);
                out.push(ethernet(&flow.guest_mac, &GATEWAY_MAC, 0x0800, &ip_packet(17, &flow.reply_src, &flow.guest_ip, &udp)));
            }
        }
        self.tcp.retain(|c| {
            let timeout = if matches!(c.transport, Transport::TimeWait) { TIME_WAIT_US } else { IDLE_CLOSE_US };
            !c.closed() && now_us.wrapping_sub(c.last_activity_us) < timeout
        });
        self.udp.retain(|f| now_us.wrapping_sub(f.last_activity_us) < IDLE_CLOSE_US);
        out
    }
}

fn host_resolver() -> [u8; 4] {
    if let Ok(conf) = std::fs::read_to_string("/etc/resolv.conf") {
        for line in conf.lines() {
            if let Some(rest) = line.trim().strip_prefix("nameserver ") {
                if let Ok(a) = rest.trim().parse::<Ipv4Addr>() { return a.octets(); }
            }
        }
    }
    [1, 1, 1, 1]
}

#[cfg(test)]
mod tests;
