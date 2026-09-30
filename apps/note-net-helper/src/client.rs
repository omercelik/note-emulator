//! `note-emu`'s side of a setup-address lease. The listener fd is the helper's;
//! this process accepts connections and, on helper disconnect, closes it.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::os::fd::FromRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::book::Endpoint;
use crate::passfd::{read_line, recv_with_fd, send_with_fd};

#[derive(Debug)]
pub struct HelperError {
    pub code: String,
    pub message: String,
    pub owner: Option<String>,
}

impl std::fmt::Display for HelperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// Why shared mode is not up. `gateway` / `mask`, when present, are the vmnet
/// interface parameters, not a guest address.
#[derive(Debug)]
pub struct SharedRefusal {
    pub code: String,
    pub message: String,
    pub gateway: Option<[u8; 4]>,
    pub mask: Option<[u8; 4]>,
}

impl std::fmt::Display for SharedRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// Ask the helper to start vmnet shared mode. Always a refusal: this slice
/// does not accept a guest address from the helper, and a listening socket
/// is rejected.
pub fn request_shared(helper: &Path) -> SharedRefusal {
    let instance = uuid_v4();
    let nonce = hex_nonce();
    let sock = match connect(helper) {
        Ok(sock) => sock,
        Err(err) => return refusal_io(err),
    };
    if let Err(err) = sock.set_read_timeout(Some(Duration::from_secs(12))) {
        return refusal_io(io_err(err));
    }
    if let Err(err) = sock.set_write_timeout(Some(Duration::from_secs(2))) {
        return refusal_io(io_err(err));
    }
    let line = format!(
        "{{\"op\":\"shared\",\"instance\":\"{instance}\",\"nonce\":\"{nonce}\",\"pid\":{}}}\n",
        std::process::id()
    );
    let (value, fd) = match exchange(&sock, &line) {
        Ok(pair) => pair,
        Err(err) => return SharedRefusal { code: err.code, message: err.message, gateway: None, mask: None },
    };
    if fd.is_some() {
        return SharedRefusal {
            code: "VmnetInactive".into(),
            message: "helper passed a socket; shared mode is not a port forward".into(),
            gateway: None,
            mask: None,
        };
    }
    if value.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        return SharedRefusal {
            code: "VmnetInactive".into(),
            message: "helper claimed shared mode was up; no guest address was accepted".into(),
            gateway: None,
            mask: None,
        };
    }
    let err = from_json(&value);
    SharedRefusal {
        code: err.code,
        message: err.message,
        gateway: value.get("vmnet_gateway").and_then(|v| v.as_str()).and_then(parse_ipv4),
        mask: value.get("vmnet_mask").and_then(|v| v.as_str()).and_then(parse_ipv4),
    }
}

/// The lease protocol the helper at `helper` speaks, or `None` when nothing answers there.
/// A helper from before the `version` request refuses it, and counts as protocol 1.
pub fn installed_protocol(helper: &Path) -> Option<u32> {
    let sock = UnixStream::connect(helper).ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    sock.set_write_timeout(Some(Duration::from_secs(2))).ok()?;
    let line = format!(
        "{{\"op\":\"version\",\"instance\":\"{}\",\"nonce\":\"{}\",\"pid\":{}}}\n",
        uuid_v4(),
        hex_nonce(),
        std::process::id()
    );
    let (value, _) = exchange(&sock, &line).ok()?;
    if value.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        value.get("protocol").and_then(|v| v.as_u64()).map(|v| v as u32)
    } else if value.get("error").and_then(|v| v.as_str()) == Some("BadRequest") {
        Some(1)
    } else {
        None
    }
}

fn refusal_io(err: HelperError) -> SharedRefusal {
    SharedRefusal { code: err.code, message: err.message, gateway: None, mask: None }
}

fn parse_ipv4(text: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = text.split('.');
    for slot in &mut out {
        *slot = parts.next()?.parse().ok()?;
    }
    if parts.next().is_some() { None } else { Some(out) }
}

pub struct Session {
    sock: UnixStream,
    listener: Option<TcpListener>,
    control: UnixListener,
    instance: String,
    nonce: String,
    /// True while this process still has the listening socket open, including
    /// after [`Session::take_listener`] handed it to the machine.
    holding: bool,
}

impl Session {
    pub fn acquire(helper: &Path, control: &Path) -> Result<Session, HelperError> {
        Self::acquire_endpoint(helper, control, Endpoint::Setup)
    }

    /// Lease one endpoint. Each endpoint needs its own session and its own `control` socket.
    pub fn acquire_endpoint(helper: &Path, control: &Path, endpoint: Endpoint) -> Result<Session, HelperError> {
        if let Some(parent) = control.parent() {
            fs::create_dir_all(parent).map_err(io_err)?;
        }
        let _ = fs::remove_file(control);
        if control.as_os_str().len() >= 100 {
            return Err(HelperError { code: "BadRequest".into(), message: format!("{} is too long for a unix socket", control.display()), owner: None });
        }
        let listener = UnixListener::bind(control).map_err(io_err)?;
        listener.set_nonblocking(true).map_err(io_err)?;
        let instance = uuid_v4();
        let nonce = hex_nonce();
        let sock = connect(helper)?;
        sock.set_read_timeout(Some(Duration::from_secs(2))).map_err(io_err)?;
        sock.set_write_timeout(Some(Duration::from_secs(2))).map_err(io_err)?;
        let line = format!(
            "{{\"op\":\"lease\",\"instance\":\"{instance}\",\"nonce\":\"{nonce}\",\"pid\":{},\"control\":\"{}\",\"endpoint\":\"{}\"}}\n",
            std::process::id(),
            control.display(),
            endpoint.name()
        );
        let (value, fd) = exchange(&sock, &line)?;
        if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            return Err(from_json(&value));
        }
        let fd = fd.ok_or_else(|| HelperError { code: "HelperUnavailable".into(), message: "helper granted a lease without a listening socket".into(), owner: None })?;
        let tcp = unsafe { TcpListener::from_raw_fd(fd) };
        let session = Session { sock, listener: Some(tcp), control: listener, instance, nonce, holding: true };
        // A helper from before endpoints ignores the field and leases the setup address.
        let want = format!("{}:80", endpoint.ip_text());
        if value.get("endpoint").and_then(|v| v.as_str()) != Some(want.as_str()) {
            let _ = session.release();
            return Err(HelperError {
                code: "HelperOutdated".into(),
                message: format!("the installed helper cannot lease {want}; reinstall it from the app"),
                owner: None,
            });
        }
        Ok(session)
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }

    pub fn take_listener(&mut self) -> Option<TcpListener> {
        self.listener.take()
    }

    /// The helper connection dropped or the lease was revoked. The listening
    /// socket, wherever it now lives, must be closed by the caller as well.
    pub fn mark_closed(&mut self) {
        self.holding = false;
        self.listener.take();
    }

    pub fn holding(&self) -> bool {
        self.holding
    }

    pub fn heartbeat(&mut self) -> Result<(), HelperError> {
        let line = self.line("heartbeat");
        match exchange(&self.sock, &line) {
            Ok((value, _)) if value.get("ok").and_then(|v| v.as_bool()) == Some(true) => Ok(()),
            Ok((value, _)) => {
                self.mark_closed();
                Err(from_json(&value))
            }
            Err(err) => {
                self.mark_closed();
                Err(err)
            }
        }
    }

    /// Answer one reconcile probe from a helper that restarted. `Ok(false)` means
    /// nobody connected.
    pub fn poll_control(&mut self) -> io::Result<bool> {
        let (mut sock, _) = match self.control.accept() {
            Ok(pair) => pair,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            // The prober gave up while queued: nobody to answer.
            Err(err) if matches!(err.kind(), io::ErrorKind::ConnectionAborted | io::ErrorKind::Interrupted) => return Ok(false),
            Err(err) => return Err(err),
        };
        // BSD sockets inherit O_NONBLOCK from the listener; the probe's line may not have
        // arrived yet, so wait for it with a timeout instead of failing with WouldBlock.
        sock.set_nonblocking(false)?;
        sock.set_read_timeout(Some(Duration::from_secs(1)))?;
        let line = read_line(&mut sock, 4096)?;
        let value: serde_json::Value = serde_json::from_str(&line).unwrap_or(serde_json::Value::Null);
        let mine = value.get("instance").and_then(|v| v.as_str()) == Some(self.instance.as_str())
            && value.get("nonce").and_then(|v| v.as_str()) == Some(self.nonce.as_str());
        let holding = mine && self.holding;
        write!(sock, "{{\"holding\":{holding}}}\n")?;
        Ok(true)
    }

    pub fn release(mut self) -> Result<(), HelperError> {
        self.listener.take();
        self.holding = false;
        let line = self.line("release");
        let (value, _) = exchange(&self.sock, &line)?;
        if value.get("ok").and_then(|v| v.as_bool()) == Some(true) { Ok(()) } else { Err(from_json(&value)) }
    }

    fn line(&self, op: &str) -> String {
        format!("{{\"op\":\"{op}\",\"instance\":\"{}\",\"nonce\":\"{}\",\"pid\":{}}}\n", self.instance, self.nonce, std::process::id())
    }
}

fn exchange(sock: &UnixStream, line: &str) -> Result<(serde_json::Value, Option<std::os::fd::RawFd>), HelperError> {
    send_with_fd(sock, line.as_bytes(), None).map_err(io_err)?;
    let (buf, fd) = recv_with_fd(sock).map_err(io_err)?;
    let text = std::str::from_utf8(&buf).map_err(|_| HelperError { code: "BadRequest".into(), message: "helper response is not UTF-8".into(), owner: None })?;
    let text = text.trim_end_matches(['\n', '\r']);
    let value = serde_json::from_str(text).map_err(|_| HelperError { code: "BadRequest".into(), message: format!("helper response is not JSON: {text}"), owner: None })?;
    Ok((value, fd))
}

fn from_json(value: &serde_json::Value) -> HelperError {
    HelperError {
        code: value.get("error").and_then(|v| v.as_str()).unwrap_or("HelperUnavailable").to_string(),
        message: value.get("message").and_then(|v| v.as_str()).unwrap_or("helper refused the lease").to_string(),
        owner: value.get("owner").and_then(|v| v.as_str()).map(str::to_string),
    }
}

fn io_err(err: io::Error) -> HelperError {
    let code = if err.kind() == io::ErrorKind::NotFound || err.kind() == io::ErrorKind::ConnectionRefused {
        "HelperUnavailable"
    } else {
        "HelperUnavailable"
    };
    HelperError { code: code.into(), message: err.to_string(), owner: None }
}

fn connect(path: &Path) -> Result<UnixStream, HelperError> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match UnixStream::connect(path) {
            Ok(sock) => return Ok(sock),
            Err(err) if Instant::now() < deadline && matches!(err.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(err) => return Err(io_err(err)),
        }
    }
}

fn uuid_v4() -> String {
    let mut b = random(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

fn hex_nonce() -> String {
    random(16).iter().map(|b| format!("{b:02x}")).collect()
}

fn random(n: usize) -> Vec<u8> {
    let mut buf = vec![0; n];
    if File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).is_err() {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(1);
        for (i, byte) in buf.iter_mut().enumerate() {
            *byte = ((t >> ((i % 8) * 8)) as u8).wrapping_add(i as u8);
        }
    }
    buf
}
