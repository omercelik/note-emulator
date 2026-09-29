//! `--gdb PORT`: one GDB remote connection on loopback, driving `note_machine::gdb`.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::time::Duration;

use note_machine::gdb::{frame, Action, Framer, Gdb, Incoming, StopReason};
use note_machine::NoteMachine;

pub struct GdbServe {
    listener: TcpListener,
    conn: Option<TcpStream>,
    framer: Framer,
    gdb: Gdb,
    /// All-stop: while halted the machine does not run and virtual time stands.
    pub halted: bool,
    /// A resume is in flight; the next DebugStop or Ctrl-C answers it.
    waiting_stop: bool,
    pub kill: bool,
}

impl GdbServe {
    pub fn start(nm: &mut NoteMachine, port: u16, wait: bool) -> Result<GdbServe, String> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|e| format!("--gdb {port}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        eprintln!("[note-emu] gdb: target remote 127.0.0.1:{port}{}", if wait { " (halted until GDB continues)" } else { "" });
        Ok(GdbServe { listener, conn: None, framer: Framer::default(), gdb: Gdb::attach(nm), halted: wait, waiting_stop: false, kill: false })
    }

    fn send(&mut self, payload: &str) {
        if let Some(c) = &mut self.conn {
            if c.write_all(&frame(payload)).is_err() {
                self.conn = None;
            }
        }
    }

    /// Accept a debugger, read what it sent, and act on it. While halted this waits up to
    /// `wait` for input so the host loop does not spin.
    pub fn service(&mut self, nm: &mut NoteMachine, wait: Duration) {
        if self.conn.is_none() {
            match self.listener.accept() {
                Ok((sock, peer)) => {
                    let _ = sock.set_nodelay(true);
                    eprintln!("[note-emu] gdb: connected from {peer}");
                    self.conn = Some(sock);
                    self.halted = true; // GDB expects an all-stop target on attach
                    self.waiting_stop = false;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::ConnectionAborted => {}
                Err(e) => eprintln!("[note-emu] gdb: accept: {e}"),
            }
        }
        let Some(conn) = &mut self.conn else {
            if self.halted {
                std::thread::sleep(wait);
            }
            return;
        };
        let _ = conn.set_read_timeout(Some(if self.halted { wait } else { Duration::from_micros(1) }));
        let mut buf = [0u8; 4096];
        match conn.read(&mut buf) {
            Ok(0) => {
                eprintln!("[note-emu] gdb: disconnected; resuming");
                self.conn = None;
                self.halted = false;
                return;
            }
            Ok(n) => self.framer.feed(&buf[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => {
                self.conn = None;
                self.halted = false;
                return;
            }
        }
        while let Some(incoming) = self.framer.next() {
            match incoming {
                Incoming::Interrupt => {
                    if !self.halted {
                        self.halted = true;
                        self.waiting_stop = false;
                        let reply = self.gdb.stop_reply(StopReason::Interrupt);
                        self.send(&reply);
                    }
                }
                Incoming::Packet(p) => {
                    if let Some(c) = &mut self.conn {
                        let _ = c.write_all(b"+");
                    }
                    match self.gdb.handle(nm, &p) {
                        Action::Reply(r) => self.send(&r),
                        Action::Continue => {
                            self.gdb.prepare_resume(nm, false);
                            self.halted = false;
                            self.waiting_stop = true;
                        }
                        Action::Step => {
                            self.gdb.prepare_resume(nm, true);
                            self.halted = false;
                            self.waiting_stop = true;
                        }
                        Action::Detach => {
                            self.send("OK");
                            self.conn = None;
                            self.halted = false;
                        }
                        Action::Kill => {
                            self.conn = None;
                            self.kill = true;
                        }
                    }
                }
            }
        }
    }

    /// Check for a breakpoint hit or finished step after a run that may have stopped early
    /// (the AVD loop runs the machine through the session, which does not report why).
    pub fn poll_stop(&mut self) {
        if let Some(reason) = self.gdb.take_stop() {
            self.halted = true;
            if self.waiting_stop {
                self.waiting_stop = false;
                let reply = self.gdb.stop_reply(reason);
                self.send(&reply);
            }
        }
    }

    /// The machine stopped on a breakpoint or finished a step.
    pub fn on_debug_stop(&mut self) {
        self.halted = true;
        if let Some(reason) = self.gdb.take_stop() {
            if self.waiting_stop {
                self.waiting_stop = false;
                let reply = self.gdb.stop_reply(reason);
                self.send(&reply);
            }
        }
    }
}
