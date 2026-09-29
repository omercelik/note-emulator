//! Lease decisions for the single setup endpoint. No sockets and no process
//! checks: the server supplies a [`HostView`] and a liveness function.

use serde::{Deserialize, Serialize};

pub const SETUP_IP: [u8; 4] = [192, 168, 4, 1];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Auth {
    Missing,
    Authorized,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Addr {
    pub ip: [u8; 4],
    pub prefix: u8,
    pub iface: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub dest: [u8; 4],
    pub prefix: u8,
    pub iface: String,
}

#[derive(Clone, Debug)]
pub struct HostView {
    pub auth: Auth,
    pub addrs: Vec<Addr>,
    pub routes: Vec<Route>,
    pub foreign_listener: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    AuthorizationRequired,
    AuthorizationCancelled,
    AddressInUse,
    AliasConflict,
    RouteConflict,
    EndpointOccupied,
    HelperUnavailable,
    /// `vmnet_start_interface` refused shared mode. The message is the status.
    VmnetDenied,
    /// The interface did not yield a guest address, so shared mode is not up.
    VmnetInactive,
    BadRequest,
}

pub fn code_str(code: Code) -> &'static str {
    match code {
        Code::AuthorizationRequired => "AuthorizationRequired",
        Code::AuthorizationCancelled => "AuthorizationCancelled",
        Code::AddressInUse => "AddressInUse",
        Code::AliasConflict => "AliasConflict",
        Code::RouteConflict => "RouteConflict",
        Code::EndpointOccupied => "EndpointOccupied",
        Code::HelperUnavailable => "HelperUnavailable",
        Code::VmnetDenied => "VmnetDenied",
        Code::VmnetInactive => "VmnetInactive",
        Code::BadRequest => "BadRequest",
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub instance: String,
    pub nonce: String,
    pub pid: u32,
    pub control: String,
    pub alias_owned: bool,
    pub holding: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Grant { own_alias: bool },
    AlreadyHeld,
    Deny { code: Code, owner: Option<String>, message: String },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Book {
    pub leases: Vec<Lease>,
}

impl Book {
    pub fn request(&self, view: &HostView, instance: &str) -> Verdict {
        match view.auth {
            Auth::Cancelled => return deny(Code::AuthorizationCancelled, None, "authorization was cancelled; other network modes stay available"),
            Auth::Missing => return deny(Code::AuthorizationRequired, None, "helper is not authorized; run `note-net-helper authorize`"),
            Auth::Authorized => {}
        }
        if let Some(lease) = self.leases.first() {
            if lease.instance == instance {
                return Verdict::AlreadyHeld;
            }
            return deny(Code::AddressInUse, Some(lease.instance.clone()), format!("192.168.4.1:80 is leased to {}", lease.instance));
        }
        if view.addrs.iter().any(|a| a.ip == SETUP_IP && a.iface != "lo0") {
            return deny(Code::AliasConflict, None, "192.168.4.1 is already assigned on another interface");
        }
        if view.addrs.iter().any(|a| a.ip == SETUP_IP) {
            return deny(Code::AliasConflict, None, "192.168.4.1 is already on lo0 and is not a helper lease");
        }
        if let Some(route) = view.routes.iter().find(|r| r.iface != "lo0" && contains(r, SETUP_IP)) {
            return deny(Code::RouteConflict, None, format!("192.168.4.1 overlaps a route on {}", route.iface));
        }
        if view.foreign_listener {
            return deny(Code::EndpointOccupied, None, "something is already listening on 192.168.4.1:80");
        }
        Verdict::Grant { own_alias: true }
    }

    pub fn insert(&mut self, lease: Lease) {
        self.leases.retain(|l| l.instance != lease.instance);
        self.leases.push(lease);
    }

    pub fn heartbeat(&self, instance: &str, nonce: &str) -> bool {
        self.leases.iter().any(|l| l.instance == instance && l.nonce == nonce)
    }

    /// Drop leases whose pid the platform reports gone. Heartbeat age is not consulted.
    pub fn reap(&mut self, alive: impl Fn(u32) -> bool) -> Vec<Lease> {
        let mut dead = Vec::new();
        self.leases.retain(|lease| {
            if alive(lease.pid) {
                true
            } else {
                dead.push(lease.clone());
                false
            }
        });
        dead
    }

    pub fn release(&mut self, instance: &str, nonce: &str) -> Result<Lease, Code> {
        let Some(i) = self.leases.iter().position(|l| l.instance == instance && l.nonce == nonce) else {
            return Err(Code::BadRequest);
        };
        Ok(self.leases.remove(i))
    }

    /// `holding` is what the runtime says about its listener. `false` drops the
    /// lease so the endpoint can be reused; `true` keeps it exclusive.
    pub fn reconcile(&mut self, instance: &str, nonce: &str, holding: bool) -> Result<Option<Lease>, Code> {
        let Some(i) = self.leases.iter().position(|l| l.instance == instance && l.nonce == nonce) else {
            return Err(Code::BadRequest);
        };
        if holding {
            self.leases[i].holding = true;
            Ok(None)
        } else {
            Ok(Some(self.leases.remove(i)))
        }
    }
}

fn deny(code: Code, owner: Option<String>, message: impl Into<String>) -> Verdict {
    Verdict::Deny { code, owner, message: message.into() }
}

fn contains(route: &Route, ip: [u8; 4]) -> bool {
    if route.prefix == 0 || route.prefix > 32 {
        return false;
    }
    let mask = u32::MAX << (32 - route.prefix);
    let ip = u32::from_be_bytes(ip);
    let dest = u32::from_be_bytes(route.dest);
    ip & mask == dest & mask
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(auth: Auth) -> HostView {
        HostView { auth, addrs: Vec::new(), routes: Vec::new(), foreign_listener: false }
    }

    fn lease(instance: &str) -> Lease {
        Lease { instance: instance.into(), nonce: "ab".repeat(16), pid: 10, control: "/tmp/helper.sock".into(), alias_owned: true, holding: true }
    }

    #[test]
    fn denial_cancellation_conflicts_and_second_owner() {
        assert!(matches!(Book::default().request(&view(Auth::Missing), "a"), Verdict::Deny { code: Code::AuthorizationRequired, .. }));
        assert!(matches!(Book::default().request(&view(Auth::Cancelled), "a"), Verdict::Deny { code: Code::AuthorizationCancelled, .. }));

        let mut aliased = view(Auth::Authorized);
        aliased.addrs.push(Addr { ip: SETUP_IP, prefix: 32, iface: "en0".into() });
        assert!(matches!(Book::default().request(&aliased, "a"), Verdict::Deny { code: Code::AliasConflict, .. }));

        let mut preexisting = view(Auth::Authorized);
        preexisting.addrs.push(Addr { ip: SETUP_IP, prefix: 32, iface: "lo0".into() });
        assert!(matches!(Book::default().request(&preexisting, "a"), Verdict::Deny { code: Code::AliasConflict, .. }));

        let mut vpn = view(Auth::Authorized);
        vpn.routes.push(Route { dest: [192, 168, 0, 0], prefix: 16, iface: "utun0".into() });
        assert!(matches!(Book::default().request(&vpn, "a"), Verdict::Deny { code: Code::RouteConflict, .. }));

        let mut occupied = view(Auth::Authorized);
        occupied.foreign_listener = true;
        assert!(matches!(Book::default().request(&occupied, "a"), Verdict::Deny { code: Code::EndpointOccupied, .. }));

        let mut book = Book::default();
        book.insert(lease("owner"));
        match book.request(&view(Auth::Authorized), "other") {
            Verdict::Deny { code: Code::AddressInUse, owner, .. } => assert_eq!(owner.as_deref(), Some("owner")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(book.request(&view(Auth::Authorized), "owner"), Verdict::AlreadyHeld));
        assert!(matches!(Book::default().request(&view(Auth::Authorized), "a"), Verdict::Grant { own_alias: true }));
    }

    #[test]
    fn a_default_route_is_not_a_conflict_and_a_tighter_one_is() {
        let mut routes = view(Auth::Authorized);
        routes.routes.push(Route { dest: [0, 0, 0, 0], prefix: 0, iface: "en0".into() });
        assert!(matches!(Book::default().request(&routes, "a"), Verdict::Grant { .. }));
        routes.routes.push(Route { dest: [192, 168, 4, 0], prefix: 22, iface: "utun3".into() });
        assert!(matches!(Book::default().request(&routes, "a"), Verdict::Deny { code: Code::RouteConflict, .. }));
        routes.routes.pop();
        routes.routes.push(Route { dest: [192, 168, 8, 0], prefix: 22, iface: "utun3".into() });
        assert!(matches!(Book::default().request(&routes, "a"), Verdict::Grant { .. }));
    }

    #[test]
    fn missed_heartbeats_do_not_drop_a_live_pid_and_a_dead_one_does() {
        let mut book = Book::default();
        book.insert(lease("owner"));
        assert!(book.reap(|_| true).is_empty());
        assert_eq!(book.leases.len(), 1);
        let dead = book.reap(|_| false);
        assert_eq!(dead.len(), 1);
        assert!(dead[0].alias_owned);
        assert!(book.leases.is_empty());
    }

    #[test]
    fn reconcile_keeps_a_holder_and_releases_a_closed_listener() {
        let mut book = Book::default();
        book.insert(lease("owner"));
        assert!(book.reconcile("owner", &"ab".repeat(16), true).unwrap().is_none());
        assert_eq!(book.leases.len(), 1);
        let removed = book.reconcile("owner", &"ab".repeat(16), false).unwrap().unwrap();
        assert!(removed.alias_owned);
        assert!(book.leases.is_empty());
    }
}
