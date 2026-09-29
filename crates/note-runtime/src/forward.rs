//! User-mode inbound forward syntax (Spec §8.2). Loopback only: a wildcard bind
//! would publish the guest on every interface, which decision D10 rejects.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardSpec {
    pub bind: SocketAddr,
    pub guest_port: u16,
}

/// `8080`, `8080:80`, or `127.0.0.1:8080:80`. The guest port defaults to 80.
pub fn parse_forward(spec: &str) -> Result<ForwardSpec, String> {
    let parts: Vec<&str> = spec.split(':').collect();
    let (ip, host_port, guest_port) = match parts.as_slice() {
        [port] => (IpAddr::V4(Ipv4Addr::LOCALHOST), parse_port(port, spec)?, 80),
        [port, guest] => (IpAddr::V4(Ipv4Addr::LOCALHOST), parse_port(port, spec)?, parse_port(guest, spec)?),
        [host, port, guest] => {
            let ip: IpAddr = host.parse().map_err(|_| format!("--forward {spec:?}: {host:?} is not an IP address"))?;
            (ip, parse_port(port, spec)?, parse_port(guest, spec)?)
        }
        _ => return Err(format!("--forward {spec:?}: expected <port>, <port>:<guest>, or <ip>:<port>:<guest>")),
    };
    if !ip.is_loopback() {
        return Err(format!("--forward {spec:?}: user forwards bind a loopback address; a wildcard or LAN address would expose the guest"));
    }
    if host_port == 0 && parts.len() == 1 {
        return Err(format!("--forward {spec:?}: port 0 is not a preferred port"));
    }
    Ok(ForwardSpec { bind: SocketAddr::new(ip, host_port), guest_port })
}

fn parse_port(text: &str, spec: &str) -> Result<u16, String> {
    text.parse::<u16>().map_err(|_| format!("--forward {spec:?}: {text:?} is not a port"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_and_the_loopback_rule() {
        assert_eq!(parse_forward("8080").unwrap(), ForwardSpec { bind: "127.0.0.1:8080".parse().unwrap(), guest_port: 80 });
        assert_eq!(parse_forward("8080:443").unwrap().guest_port, 443);
        assert_eq!(parse_forward("127.0.0.1:9:80").unwrap().bind.port(), 9);
        assert!(parse_forward("0.0.0.0:80:80").is_err());
        assert!(parse_forward("192.168.1.5:80:80").is_err());
        assert!(parse_forward("nope").is_err());
        assert!(parse_forward("0").is_err());
    }
}
