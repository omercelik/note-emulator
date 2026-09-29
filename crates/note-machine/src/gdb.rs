//! GDB remote serial protocol for a NOTE machine (DEV-01): both LX7 cores as threads 1 and 2,
//! the register layout `xtensa-esp32s3-elf-gdb` expects (`maint print remote-registers`,
//! esp-gdb 16.3: 212 registers, 944-byte `g` packet), memory access outside the peripheral
//! window, breakpoints (`Z0`/`Z1`) and single-step. All-stop: while halted, virtual time stands.
//!
//! Breakpoints are an engine observer that stops before the instruction at a watched PC; it puts
//! the machine on the per-instruction path, so it is attached only for debug sessions.

use std::sync::{Arc, Mutex};

use esp_soc::{Ctx, Observer, Stop, Wants};

use crate::NoteMachine;

/// Size in bytes of each register in the `g` packet, by GDB register number.
fn reg_size(n: usize) -> usize {
    if (120..=127).contains(&n) { 16 } else { 4 }
}
pub const NUM_REGS: usize = 212;

/// Why the target stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// Halted by the debugger (attach with `--gdb-wait`, Ctrl-C).
    Interrupt,
    Breakpoint { core: usize, pc: u32 },
    Step { core: usize, pc: u32 },
}

#[derive(Default)]
struct Shared {
    breakpoints: Vec<u32>,
    step_core: Option<usize>,
    /// The instruction a resume starts on: it runs once even if it is a breakpoint.
    skip: [Option<u32>; 2],
    stop: Option<StopReason>,
}

struct GdbObserver(Arc<Mutex<Shared>>);

impl Observer<esp32s3::S3> for GdbObserver {
    fn name(&self) -> &'static str {
        "gdb"
    }
    fn wants(&self) -> Wants {
        Wants::INSN | Wants::IDLE_PC
    }
    fn on_insn(&mut self, _cx: &Ctx, core: usize, _cpu: &xtensa_lx7::state::Cpu, _bus: &mut esp32s3::bus::SocBus, pc: u32) -> Option<Stop> {
        let mut s = self.0.lock().unwrap();
        if let Some(slot) = s.skip.get_mut(core) {
            if *slot == Some(pc) {
                *slot = None;
                return None;
            }
            *slot = None;
        }
        if s.breakpoints.contains(&pc) {
            s.stop = Some(StopReason::Breakpoint { core, pc });
            return Some(Stop::Breakpoint(pc));
        }
        None
    }
    fn after_insn(&mut self, _cx: &Ctx, core: usize, cpu: &xtensa_lx7::state::Cpu, _bus: &mut esp32s3::bus::SocBus) -> Option<Stop> {
        let mut s = self.0.lock().unwrap();
        if s.step_core == Some(core) {
            s.step_core = None;
            s.stop = Some(StopReason::Step { core, pc: cpu.pc });
            return Some(Stop::Breakpoint(cpu.pc));
        }
        None
    }
}

/// What the runtime must do after a packet.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Reply(String),
    /// Resume all cores until a stop; reply with [`Gdb::stop_reply`] when it happens.
    Continue,
    /// Execute one instruction on `core`, then stop.
    Step,
    Detach,
    Kill,
}

pub struct Gdb {
    shared: Arc<Mutex<Shared>>,
    /// Thread for `g`/`p`/`m` (Hg) and for `s` (Hc), as a core index.
    reg_core: usize,
    run_core: usize,
}

impl Gdb {
    /// Attach the debug observer to `nm`.
    pub fn attach(nm: &mut NoteMachine) -> Gdb {
        let shared = Arc::new(Mutex::new(Shared::default()));
        nm.m.add_observer(Box::new(GdbObserver(shared.clone())));
        Gdb { shared, reg_core: 0, run_core: 0 }
    }

    /// The stop recorded by the observer during the last run, if any.
    pub fn take_stop(&mut self) -> Option<StopReason> {
        self.shared.lock().unwrap().stop.take()
    }

    pub fn stop_reply(&mut self, reason: StopReason) -> String {
        match reason {
            StopReason::Interrupt => format!("T02thread:{:x};", self.reg_core + 1),
            StopReason::Breakpoint { core, .. } => {
                self.reg_core = core;
                format!("T05thread:{:x};swbreak:;", core + 1)
            }
            StopReason::Step { core, .. } => {
                self.reg_core = core;
                format!("T05thread:{:x};", core + 1)
            }
        }
    }

    /// Prepare a resume: each core may run the instruction it is stopped at, even if it is a
    /// breakpoint.
    pub fn prepare_resume(&mut self, nm: &NoteMachine, step: bool) {
        let mut s = self.shared.lock().unwrap();
        for (i, core) in nm.m.cores.iter().enumerate().take(2) {
            s.skip[i] = Some(core.pc);
        }
        s.step_core = step.then_some(self.run_core);
    }

    pub fn handle(&mut self, nm: &mut NoteMachine, packet: &str) -> Action {
        let reply = |s: &str| Action::Reply(s.to_string());
        let (cmd, rest) = packet.split_at(packet.chars().next().map_or(0, char::len_utf8));
        match cmd {
            "?" => Action::Reply(format!("T02thread:{:x};", self.reg_core + 1)),
            "g" => Action::Reply((0..NUM_REGS).map(|n| reg_hex(nm, self.reg_core, n)).collect()),
            "p" => match usize::from_str_radix(rest, 16) {
                Ok(n) if n < NUM_REGS => Action::Reply(reg_hex(nm, self.reg_core, n)),
                _ => reply("E01"),
            },
            "P" => match rest.split_once('=').and_then(|(n, v)| Some((usize::from_str_radix(n, 16).ok()?, from_hex(v)?))) {
                Some((n, bytes)) if set_reg(nm, self.reg_core, n, &bytes) => reply("OK"),
                _ => reply("E01"),
            },
            "m" => match parse_addr_len(rest) {
                Some((addr, len)) if len <= 0x1000 => match read_mem(nm, addr, len) {
                    Some(bytes) => Action::Reply(to_hex(&bytes)),
                    None => reply("E14"),
                },
                _ => reply("E01"),
            },
            "M" => {
                let Some((spec, data)) = rest.split_once(':') else { return reply("E01") };
                match (parse_addr_len(spec), from_hex(data)) {
                    (Some((addr, len)), Some(bytes)) if bytes.len() == len && write_mem(nm, addr, &bytes) => reply("OK"),
                    _ => reply("E14"),
                }
            }
            "Z" | "z" => {
                let mut f = rest.split(',');
                let (Some(kind), Some(addr)) = (f.next(), f.next().and_then(|a| u32::from_str_radix(a, 16).ok())) else {
                    return reply("E01");
                };
                if kind != "0" && kind != "1" {
                    return reply(""); // watchpoints: not supported
                }
                let mut s = self.shared.lock().unwrap();
                if cmd == "Z" {
                    if !s.breakpoints.contains(&addr) {
                        s.breakpoints.push(addr);
                    }
                } else {
                    s.breakpoints.retain(|&a| a != addr);
                }
                reply("OK")
            }
            "c" => {
                if let Ok(addr) = u32::from_str_radix(rest, 16) {
                    nm.m.cores[self.run_core].pc = addr;
                }
                Action::Continue
            }
            "s" => {
                if let Ok(addr) = u32::from_str_radix(rest, 16) {
                    nm.m.cores[self.run_core].pc = addr;
                }
                Action::Step
            }
            "H" => {
                let (op, thread) = rest.split_at(rest.len().min(1));
                let core = match i64::from_str_radix(thread, 16) {
                    Ok(t) if t >= 1 && (t as usize) <= nm.m.cores.len() => Some(t as usize - 1),
                    Ok(0) | Ok(-1) => None,
                    _ => return reply("E01"),
                };
                if let Some(core) = core {
                    if op == "g" { self.reg_core = core } else { self.run_core = core }
                }
                reply("OK")
            }
            "T" => match usize::from_str_radix(rest, 16) {
                Ok(t) if t >= 1 && t <= nm.m.cores.len() => reply("OK"),
                _ => reply("E01"),
            },
            "k" => Action::Kill,
            "D" => Action::Detach,
            "q" => self.query(nm, rest),
            "v" => {
                if rest.starts_with("MustReplyEmpty") {
                    reply("")
                } else {
                    reply("") // vCont and friends: use c/s with Hc
                }
            }
            _ => reply(""),
        }
    }

    fn query(&self, nm: &NoteMachine, q: &str) -> Action {
        let reply = |s: String| Action::Reply(s);
        if q.starts_with("Supported") {
            reply("PacketSize=4000;swbreak+;hwbreak+".into())
        } else if q == "fThreadInfo" {
            reply(format!("m{}", (1..=nm.m.cores.len()).map(|t| format!("{t:x}")).collect::<Vec<_>>().join(",")))
        } else if q == "sThreadInfo" {
            reply("l".into())
        } else if q == "C" {
            reply(format!("QC{:x}", self.reg_core + 1))
        } else if q.starts_with("Attached") {
            reply("1".into())
        } else if let Some(t) = q.strip_prefix("ThreadExtraInfo,") {
            let core = usize::from_str_radix(t, 16).unwrap_or(1).saturating_sub(1);
            reply(to_hex(format!("core{core} (LX7)").as_bytes()))
        } else if q.starts_with("Symbol") {
            reply("OK".into())
        } else {
            reply(String::new())
        }
    }
}

fn reg_value(nm: &NoteMachine, core: usize, n: usize) -> Option<Vec<u8>> {
    let c = &nm.m.cores[core];
    let w = |v: u32| Some(v.to_le_bytes().to_vec());
    match n {
        0 => w(c.pc),
        1..=64 => w(c.ar[n - 1]),
        65 => w(c.lbeg),
        66 => w(c.lend),
        67 => w(c.lcount),
        68 => w(c.sar),
        69 => w(c.windowbase),
        70 => w(c.windowstart),
        71 => w(c.configid[0]),
        72 => w(c.configid[1]),
        73 => w(c.ps),
        74 => w(c.threadptr),
        75 => w(c.br),
        76 => w(c.scompare1),
        77 => w(c.acclo),
        78 => w(c.acchi),
        79..=82 => w(c.m[n - 79]),
        83 => w(c.gpio_out),
        84..=99 => w(c.fr[n - 84]),
        100 => w(c.fcr),
        101 => w(c.fsr),
        102 | 103 => w(c.accx[n - 102]),
        104..=108 => w(c.qacc_h[n - 104]),
        109..=113 => w(c.qacc_l[n - 109]),
        114 => w(c.sar_byte),
        115 => w(c.fft_bit_width),
        116..=119 => w(c.ua_state[n - 116]),
        120..=127 => Some(c.qr[n - 120].to_le_bytes().to_vec()),
        129 => w(c.ibreakenable),
        130 => w(c.memctl),
        131 => w(c.atomctl),
        132 => w(c.ddr),
        133 | 134 => w(c.ibreaka[n - 133]),
        135 | 136 => w(c.dbreaka[n - 135]),
        137 | 138 => w(c.dbreakc[n - 137]),
        139..=145 => w(c.epc[n - 138]),
        146 => w(c.depc),
        147..=152 => w(c.eps[n - 145]),
        153..=159 => w(c.excsave[n - 152]),
        160 => w(c.cpenable),
        161 => w(c.interrupt),
        164 => w(c.intenable),
        165 => w(c.vecbase),
        166 => w(c.exccause),
        167 => w(c.debugcause),
        168 => w(c.ccount),
        169 => w(c.prid),
        170 => w(c.icount),
        171 => w(c.icountlevel),
        172 => w(c.excvaddr),
        173..=175 => w(c.ccompare[n - 173]),
        176..=179 => w(c.misc[n - 176]),
        _ => None,
    }
}

fn reg_hex(nm: &NoteMachine, core: usize, n: usize) -> String {
    match reg_value(nm, core, n) {
        Some(bytes) => to_hex(&bytes),
        None => "xx".repeat(reg_size(n)),
    }
}

fn set_reg(nm: &mut NoteMachine, core: usize, n: usize, bytes: &[u8]) -> bool {
    if bytes.len() != 4 {
        return false;
    }
    let v = u32::from_le_bytes(bytes.try_into().unwrap());
    let c = &mut nm.m.cores[core];
    match n {
        0 => c.pc = v,
        1..=64 => c.ar[n - 1] = v,
        65 => c.lbeg = v,
        66 => c.lend = v,
        67 => c.lcount = v,
        68 => c.sar = v,
        73 => c.ps = v,
        79..=82 => c.m[n - 79] = v,
        84..=99 => c.fr[n - 84] = v,
        _ => return false,
    }
    true
}

/// Debugger reads never touch peripheral registers (they have side effects).
fn debuggable(addr: u32, len: usize) -> bool {
    let end = addr as u64 + len as u64;
    let periph = (esp32s3::periph::PERIPH_BASE as u64, esp32s3::periph::PERIPH_END as u64);
    end <= periph.0 || addr as u64 >= periph.1
}

fn read_mem(nm: &mut NoteMachine, addr: u32, len: usize) -> Option<Vec<u8>> {
    use emu_core::Bus;
    if !debuggable(addr, len) {
        return None;
    }
    (0..len as u32).map(|i| nm.m.bus.read8_unpriced(addr.wrapping_add(i)).ok()).collect()
}

fn write_mem(nm: &mut NoteMachine, addr: u32, bytes: &[u8]) -> bool {
    use emu_core::Bus;
    debuggable(addr, bytes.len())
        && bytes.iter().enumerate().all(|(i, &b)| nm.m.bus.write8_unpriced(addr.wrapping_add(i as u32), b).is_ok())
}

fn parse_addr_len(s: &str) -> Option<(u32, usize)> {
    let (a, l) = s.split_once(',')?;
    Some((u32::from_str_radix(a, 16).ok()?, usize::from_str_radix(l, 16).ok()?))
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// Remote-protocol framing on a byte stream: `$payload#cs`, `+`/`-` acks, `0x03` interrupts.
#[derive(Default)]
pub struct Framer {
    buf: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Incoming {
    Packet(String),
    Interrupt,
}

impl Framer {
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next complete packet (checksum verified) or interrupt; acks are dropped.
    pub fn next(&mut self) -> Option<Incoming> {
        loop {
            let first = *self.buf.first()?;
            match first {
                0x03 => {
                    self.buf.remove(0);
                    return Some(Incoming::Interrupt);
                }
                b'$' => {
                    let hash = self.buf.iter().position(|&b| b == b'#')?;
                    if self.buf.len() < hash + 3 {
                        return None;
                    }
                    let payload = self.buf[1..hash].to_vec();
                    let sum = std::str::from_utf8(&self.buf[hash + 1..hash + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                    self.buf.drain(..hash + 3);
                    if sum == Some(checksum(&payload)) {
                        return Some(Incoming::Packet(String::from_utf8_lossy(&payload).into_owned()));
                    }
                }
                _ => {
                    self.buf.remove(0);
                }
            }
        }
    }
}

pub fn checksum(payload: &[u8]) -> u8 {
    payload.iter().fold(0u8, |a, &b| a.wrapping_add(b))
}

/// `$payload#cs` for a reply.
pub fn frame(payload: &str) -> Vec<u8> {
    format!("${payload}#{:02x}", checksum(payload.as_bytes())).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_checks_checksums_interrupts_and_acks() {
        let mut f = Framer::default();
        f.feed(b"+$qSupported:swbreak+#");
        assert_eq!(f.next(), None);
        f.feed(b"a3\x03$g#67$m0,4#00");
        let sum = checksum(b"qSupported:swbreak+");
        let expected = if sum == 0xa3 { Some(Incoming::Packet("qSupported:swbreak+".into())) } else { None };
        let first = f.next();
        if expected.is_some() {
            assert_eq!(first, expected);
            assert_eq!(f.next(), Some(Incoming::Interrupt));
        } else {
            assert_eq!(first, Some(Incoming::Interrupt), "bad checksum dropped");
        }
        assert_eq!(f.next(), Some(Incoming::Packet("g".into())));
        assert_eq!(f.next(), None, "bad checksum is dropped");
        assert_eq!(frame("OK"), b"$OK#9a");
    }

    #[test]
    fn g_packet_is_the_esp_gdb_layout_size() {
        let total: usize = (0..NUM_REGS).map(reg_size).sum();
        assert_eq!(total, 944);
    }

    #[test]
    fn peripheral_window_is_not_debuggable() {
        assert!(debuggable(0x3fc8_0000, 16));
        assert!(!debuggable(0x6000_4000, 4));
        assert!(!debuggable(esp32s3::periph::PERIPH_BASE - 2, 4));
    }
}
