//! What the helper is allowed to do to the host: read addresses and routes,
//! add or remove only the owned `lo0` alias, and bind the setup port.
//! Tests use [`FakePlatform`]; the binary uses [`SystemPlatform`].

use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::book::{Addr, Auth, HostView, Route};
#[cfg(test)]
use crate::book::SETUP_IP;
#[cfg(test)]
use std::collections::HashSet;

pub trait Platform: Send {
    fn view(&self) -> HostView;
    fn add_alias(&mut self) -> Result<(), String>;
    fn remove_alias(&mut self) -> Result<(), String>;
    fn bind_setup(&mut self) -> Result<TcpListener, String>;
    fn pid_alive(&self, pid: u32) -> bool;
}

#[cfg(test)]
pub struct FakePlatform {
    pub auth: Auth,
    pub addrs: Vec<Addr>,
    pub routes: Vec<Route>,
    pub foreign_listener: bool,
    pub alias: bool,
    pub bind_error: Option<String>,
    pub alive: HashSet<u32>,
    pub alias_adds: u32,
    pub alias_removes: u32,
}

#[cfg(test)]
impl FakePlatform {
    pub fn authorized() -> FakePlatform {
        let mut alive = HashSet::new();
        alive.insert(std::process::id());
        FakePlatform {
            auth: Auth::Authorized, addrs: Vec::new(), routes: Vec::new(), foreign_listener: false,
            alias: false, bind_error: None, alive, alias_adds: 0, alias_removes: 0,
        }
    }
}

#[cfg(test)]
impl Platform for FakePlatform {
    fn view(&self) -> HostView {
        let mut addrs = self.addrs.clone();
        if self.alias && !addrs.iter().any(|a| a.ip == SETUP_IP) {
            addrs.push(Addr { ip: SETUP_IP, prefix: 32, iface: "lo0".into() });
        }
        HostView { auth: self.auth, addrs, routes: self.routes.clone(), foreign_listener: self.foreign_listener }
    }

    fn add_alias(&mut self) -> Result<(), String> {
        self.alias = true;
        self.alias_adds += 1;
        Ok(())
    }

    fn remove_alias(&mut self) -> Result<(), String> {
        self.alias = false;
        self.alias_removes += 1;
        Ok(())
    }

    fn bind_setup(&mut self) -> Result<TcpListener, String> {
        if let Some(err) = &self.bind_error {
            return Err(err.clone());
        }
        // The real helper binds 192.168.4.1:80. Tests get a loopback fd so the
        // pass itself can be proven without root; the lease still names the setup URL.
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())
    }

    fn pid_alive(&self, pid: u32) -> bool {
        self.alive.contains(&pid)
    }
}

pub struct SystemPlatform {
    auth_file: PathBuf,
}

impl SystemPlatform {
    pub fn new(auth_file: impl Into<PathBuf>) -> SystemPlatform {
        SystemPlatform { auth_file: auth_file.into() }
    }

    fn auth(&self) -> Auth {
        let Ok(text) = std::fs::read_to_string(&self.auth_file) else { return Auth::Missing };
        match serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v.get("state").and_then(|s| s.as_str()).map(str::to_string)).as_deref() {
            Some("authorized") => Auth::Authorized,
            Some("cancelled") => Auth::Cancelled,
            _ => Auth::Missing,
        }
    }
}

impl Platform for SystemPlatform {
    fn view(&self) -> HostView {
        HostView { auth: self.auth(), addrs: interface_addrs(), routes: system_routes(), foreign_listener: false }
    }

    fn add_alias(&mut self) -> Result<(), String> {
        run("ifconfig", &["lo0", "alias", "192.168.4.1", "255.255.255.255"])
    }

    fn remove_alias(&mut self) -> Result<(), String> {
        run("ifconfig", &["lo0", "-alias", "192.168.4.1"])
    }

    fn bind_setup(&mut self) -> Result<TcpListener, String> {
        TcpListener::bind((Ipv4Addr::new(192, 168, 4, 1), 80)).map_err(|e| e.to_string())
    }

    fn pid_alive(&self, pid: u32) -> bool {
        let rc = unsafe { libc::kill(pid as i32, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        Err(format!("{cmd} {}: {err}", args.join(" ")).trim().to_string())
    }
}

fn interface_addrs() -> Vec<Addr> {
    let mut out = Vec::new();
    let mut head = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return out;
    }
    let mut cur = head;
    while !cur.is_null() {
        let ifa = unsafe { &*cur };
        if !ifa.ifa_addr.is_null() && unsafe { (*ifa.ifa_addr).sa_family } as i32 == libc::AF_INET {
            let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
            let ip = u32::from_be(sin.sin_addr.s_addr).to_be_bytes();
            let prefix = if ifa.ifa_netmask.is_null() {
                32
            } else {
                let mask = unsafe { &*(ifa.ifa_netmask as *const libc::sockaddr_in) };
                u32::from_be(mask.sin_addr.s_addr).count_ones() as u8
            };
            let iface = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }.to_string_lossy().into_owned();
            out.push(Addr { ip, prefix, iface });
        }
        cur = ifa.ifa_next;
    }
    unsafe { libc::freeifaddrs(head) };
    out
}

fn system_routes() -> Vec<Route> {
    Command::new("netstat").args(["-rn", "-f", "inet"]).output().ok()
        .map(|o| parse_netstat(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

pub fn parse_netstat(text: &str) -> Vec<Route> {
    let mut out = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        let Some((dest, prefix)) = parse_dest(cols[0]) else { continue };
        let Some(iface) = cols.iter().copied().find(|c| is_iface(c)) else { continue };
        out.push(Route { dest, prefix, iface: iface.to_string() });
    }
    out
}

fn parse_dest(text: &str) -> Option<([u8; 4], u8)> {
    if text == "default" {
        return Some(([0, 0, 0, 0], 0));
    }
    let (addr, prefix) = match text.split_once('/') {
        Some((addr, prefix)) => (addr, prefix.parse().ok()?),
        None => (text, 32),
    };
    let mut octets = [0u8; 4];
    let parts: Vec<&str> = addr.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    for (i, part) in parts.iter().enumerate() {
        octets[i] = part.parse().ok()?;
    }
    Some((octets, prefix))
}

fn is_iface(text: &str) -> bool {
    let (head, tail) = text.split_at(text.find(|c: char| c.is_ascii_digit()).unwrap_or(text.len()));
    matches!(head, "lo" | "en" | "utun" | "bridge" | "gif" | "stf" | "awdl" | "llw" | "ipsec" | "ap" | "vlan")
        && (tail.is_empty() || tail.bytes().all(|b| b.is_ascii_digit()))
}

pub fn write_auth(path: &Path, state: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, format!("{{\"state\":\"{state}\"}}\n")).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netstat_keeps_interface_routes_and_skips_prose() {
        let text = "\
Routing tables\n\
\n\
Internet:\n\
Destination        Gateway            Flags           Netif Expire\n\
default            192.168.1.1        UGSc              en0\n\
192.168.1          link#12            UCS               en0\n\
192.168.4.1        192.168.4.1        UH                lo0\n\
192.168.4/22       10.8.0.1           UGSc            utun3\n";
        let routes = parse_netstat(text);
        assert!(routes.iter().any(|r| r.prefix == 0 && r.iface == "en0"));
        assert!(routes.iter().any(|r| r.dest == [192, 168, 4, 1] && r.iface == "lo0" && r.prefix == 32));
        assert!(routes.iter().any(|r| r.dest == [192, 168, 4, 0] && r.prefix == 22 && r.iface == "utun3"));
    }
}
