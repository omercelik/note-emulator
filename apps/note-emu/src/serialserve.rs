//! `--serial-rfc2217 PORT`: the chip's serial port as an RFC 2217 port on loopback (DEV-02).
//! esptool and `idf.py -p rfc2217://127.0.0.1:PORT` flash through the ROM's download mode;
//! DTR/RTS drive EN/GPIO0 like the classic auto-reset circuit. NOTE boards wire USB-C straight
//! to the S3's USB-Serial/JTAG (no bridge chip), so that is the default port; UART0 is optional.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};

use note_machine::NoteMachine;
use note_runtime::rfc2217::{en_and_gpio0, Event, Rfc2217};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Port {
    UsbSerialJtag,
    Uart0,
}

pub struct SerialServe {
    port: Port,
    listener: TcpListener,
    conn: Option<(TcpStream, Rfc2217)>,
    /// EN is held low by the client: the chip does not run.
    pub in_reset: bool,
}

impl SerialServe {
    pub fn start(port: u16, which: Port) -> Result<SerialServe, String> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|e| format!("--serial-rfc2217 {port}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        eprintln!("[note-emu] serial: rfc2217://127.0.0.1:{port} ({}; DTR/RTS reset like a USB-serial bridge)",
                  if which == Port::Uart0 { "UART0" } else { "USB-Serial/JTAG" });
        Ok(SerialServe { port: which, listener, conn: None, in_reset: false })
    }

    /// Accept a client and apply what it sent: serial bytes to UART0 RX, DTR/RTS to EN/GPIO0.
    pub fn poll(&mut self, nm: &mut NoteMachine) {
        if self.conn.is_none() {
            match self.listener.accept() {
                Ok((sock, peer)) => {
                    let _ = sock.set_nodelay(true);
                    let _ = sock.set_nonblocking(true);
                    let mut stream = sock;
                    let _ = stream.write_all(&Rfc2217::greeting());
                    eprintln!("[note-emu] serial: client {peer}");
                    self.conn = Some((stream, Rfc2217::default()));
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::ConnectionAborted) => return,
                Err(e) => {
                    eprintln!("[note-emu] serial: accept: {e}");
                    return;
                }
            }
        }
        let mut closed = false;
        let mut events = Vec::new();
        if let Some((stream, proto)) = &mut self.conn {
            let mut buf = [0u8; 8192];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => { closed = true; break; }
                    Ok(n) => events.extend(proto.feed(&buf[..n])),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => { closed = true; break; }
                }
            }
            if !proto.reply.is_empty() {
                let reply = std::mem::take(&mut proto.reply);
                if stream.write_all(&reply).is_err() {
                    closed = true;
                }
            }
        }
        for ev in events {
            match ev {
                Event::Data(bytes) => {
                    if !self.in_reset {
                        match self.port {
                            Port::Uart0 => nm.uart0_input(&bytes),
                            Port::UsbSerialJtag => nm.serial_input(&bytes),
                        }
                    }
                    if std::env::var_os("NOTE_EMU_DEBUG_SERIAL").is_some() {
                        eprintln!("[serial] rx {} bytes (in_reset={}): {:02x?}", bytes.len(), self.in_reset, &bytes[..bytes.len().min(12)]);
                    }
                }
                Event::Control(lines) => {
                    let (en, gpio0) = en_and_gpio0(lines);
                    if !en {
                        self.in_reset = true;
                    } else if self.in_reset {
                        self.in_reset = false;
                        let download = !gpio0;
                        nm.pin_reset(download);
                        eprintln!("[note-emu] t={:.3}s serial: EN released, {} boot", nm.seconds(), if download { "download" } else { "SPI flash" });
                    }
                }
            }
        }
        if closed {
            eprintln!("[note-emu] serial: client left");
            self.conn = None;
            self.in_reset = false;
        }
    }

    /// This port's output for the client (the ROM loader or the application speaks here).
    pub fn send(&mut self, console: &note_machine::Console) {
        let uart0: &[u8] = match self.port {
            Port::Uart0 => &console.uart0,
            Port::UsbSerialJtag => &console.usb,
        };
        if uart0.is_empty() {
            return;
        }
        if std::env::var_os("NOTE_EMU_DEBUG_SERIAL").is_some() {
            eprintln!("[serial] tx {} bytes: {:02x?}", uart0.len(), &uart0[..uart0.len().min(16)]);
        }
        if let Some((stream, _)) = &mut self.conn {
            let data = Rfc2217::encode(uart0);
            // Non-blocking: retry short writes briefly rather than dropping protocol bytes.
            let mut off = 0;
            let mut tries = 0;
            while off < data.len() && tries < 1000 {
                match stream.write(&data[off..]) {
                    Ok(n) => off += n,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => { tries += 1; std::thread::sleep(std::time::Duration::from_micros(200)); }
                    Err(_) => break,
                }
            }
        }
    }
}
