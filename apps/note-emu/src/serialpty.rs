//! `--serial-pty PATH`: the chip's console as a pseudo-terminal for serial monitors (screen,
//! minicom, `idf.py monitor -p PATH --no-reset`). PATH becomes a symlink to the slave device.
//! macOS PTYs carry no modem lines (ADR-011), so this is monitor-only: flashing and auto-reset
//! use `--serial-rfc2217`, or `ndb boot download|normal`.

use std::ffi::CStr;
use std::os::fd::{FromRawFd, OwnedFd, AsRawFd};
use std::path::{Path, PathBuf};

use note_machine::NoteMachine;

pub struct SerialPty {
    master: OwnedFd,
    link: PathBuf,
    usb: bool,
}

impl SerialPty {
    pub fn start(link: &Path, usb: bool) -> Result<SerialPty, String> {
        let (mut master, mut slave) = (0, 0);
        let mut name = [0 as libc::c_char; 128];
        // SAFETY: openpty writes two descriptors and a NUL-terminated name into our buffers.
        let rc = unsafe { libc::openpty(&mut master, &mut slave, name.as_mut_ptr(), std::ptr::null_mut(), std::ptr::null_mut()) };
        if rc != 0 {
            return Err(format!("--serial-pty: openpty: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: both descriptors were just returned by openpty and are owned here.
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        // Raw mode on the slave: bytes pass through unchanged (no echo, no CR/LF mapping).
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(slave.as_raw_fd(), &mut t) == 0 {
                libc::cfmakeraw(&mut t);
                libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &t);
            }
            let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        let device = unsafe { CStr::from_ptr(name.as_ptr()) }.to_string_lossy().into_owned();
        let _ = std::fs::remove_file(link);
        std::os::unix::fs::symlink(&device, link).map_err(|e| format!("--serial-pty {}: {e}", link.display()))?;
        // Keep the slave open ourselves so the master does not see EOF/EIO while no monitor is attached.
        std::mem::forget(slave);
        eprintln!("[note-emu] serial pty: {} -> {device} (monitor only, no DTR/RTS)", link.display());
        Ok(SerialPty { master, link: link.to_path_buf(), usb })
    }

    /// Typed bytes from the monitor go to the chip's console input.
    pub fn poll(&mut self, nm: &mut NoteMachine) {
        let mut buf = [0u8; 4096];
        loop {
            // SAFETY: reading into a local buffer from our nonblocking master descriptor.
            let n = unsafe { libc::read(self.master.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            let bytes = &buf[..n as usize];
            if self.usb { nm.serial_input(bytes) } else { nm.uart0_input(bytes) }
        }
    }

    /// Console output to the monitor. A monitor that is not reading loses bytes, like a UART.
    pub fn send(&mut self, console: &note_machine::Console) {
        let data: &[u8] = if self.usb { &console.usb } else { &console.uart0 };
        let mut off = 0;
        while off < data.len() {
            // SAFETY: writing from a live slice to our master descriptor.
            let n = unsafe { libc::write(self.master.as_raw_fd(), data[off..].as_ptr().cast(), data.len() - off) };
            if n <= 0 {
                break;
            }
            off += n as usize;
        }
    }
}

impl Drop for SerialPty {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.link);
    }
}
