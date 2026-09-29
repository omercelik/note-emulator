//! `network.info` (Spec §8.8). Configured mode changes immediately; the active
//! address stays until the change is applied. A stopped instance is not reachable.
//! Shared mode has no address until one is learned from the guest. Nothing here
//! invents one.

use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostMode {
    Disabled,
    User,
    Setup,
    Shared,
}

impl HostMode {
    pub fn as_str(self) -> &'static str {
        match self {
            HostMode::Disabled => "disabled",
            HostMode::User => "user",
            HostMode::Setup => "setup",
            HostMode::Shared => "shared",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkInfo {
    pub configured: HostMode,
    pub active: HostMode,
    pub running: bool,
    pub guest_link: &'static str,
    pub guest_addresses: Vec<String>,
    pub bindings: Vec<String>,
    /// Advertised browser URL of the running instance. Empty once stopped.
    pub browser_url: Option<String>,
    pub access_scope: String,
    pub permission: String,
    pub reachable: bool,
    pub restart_required: bool,
    /// Multicast and mDNS are not carried (Spec §8.8).
    pub discovery: &'static str,
}

impl NetworkInfo {
    pub fn stopped(configured: HostMode) -> NetworkInfo {
        NetworkInfo {
            configured,
            active: HostMode::Disabled,
            running: false,
            guest_link: "wifi-station",
            guest_addresses: planned_addresses(configured),
            bindings: Vec::new(),
            browser_url: None,
            access_scope: scope(configured).into(),
            permission: "not-required".into(),
            reachable: false,
            restart_required: false,
            discovery: "unsupported",
        }
    }

    /// Begin a session. Replaces the active mode and its URL together.
    pub fn start(&mut self, active: HostMode, browser_url: Option<String>, bindings: Vec<String>, permission: &str) {
        self.active = active;
        self.configured = active;
        self.running = true;
        self.browser_url = browser_url;
        self.bindings = bindings;
        self.permission = permission.into();
        self.access_scope = scope(active).into();
        self.guest_addresses = planned_addresses(active);
        self.reachable = self.browser_url.is_some();
        self.restart_required = false;
    }

    /// Shared mode was selected and is not up. Drops any previous URL and address.
    pub fn deny_shared(&mut self, permission: &str) {
        self.configured = HostMode::Shared;
        self.active = HostMode::Disabled;
        self.running = false;
        self.reachable = false;
        self.browser_url = None;
        self.guest_addresses.clear();
        self.bindings.clear();
        self.permission = permission.to_string();
        self.access_scope = "shared mode is not active".into();
        self.restart_required = false;
        self.guest_link = "wifi-station";
    }

    /// Record an address learned from guest traffic. Ignored unless shared mode
    /// is the running active mode, so a failure cannot be painted reachable.
    /// Callers that feed a fixture must say so in the test name: this is not
    /// evidence of a live vmnet.
    pub fn set_discovered_guest(&mut self, address: &str) {
        if !(self.running && self.active == HostMode::Shared) {
            return;
        }
        if address.is_empty() || address.contains(['/', ' ', ':']) {
            return;
        }
        self.guest_addresses = vec![address.to_string()];
        self.browser_url = Some(format!("http://{address}/"));
        self.bindings.clear();
        self.reachable = true;
    }

    /// Record a requested mode without touching the live URL.
    pub fn configure(&mut self, mode: HostMode) {
        self.configured = mode;
        self.restart_required = self.running && mode != self.active;
    }

    pub fn stop(&mut self) {
        self.running = false;
        self.reachable = false;
        self.browser_url = None;
        self.guest_addresses.clear();
        self.bindings.clear();
        self.restart_required = false;
    }

    /// Fields `network.info` reports (Spec §8.8). `browser_url` is null unless
    /// the instance is running and reachable.
    pub fn report(&self) -> Value {
        json!({
            "configured": self.configured.as_str(),
            "active": self.active.as_str(),
            "running": self.running,
            "guest_link": self.guest_link,
            "guest_addresses": self.guest_addresses,
            "bindings": self.bindings,
            "browser_url": self.reported_url(),
            "access_scope": self.access_scope,
            "permission": self.permission,
            "reachable": self.running && self.reachable && self.browser_url.is_some(),
            "restart_required": self.restart_required,
            "discovery": self.discovery,
            "external": self.allows_external(),
        })
    }

    /// URL a client may display as reachable. Never an address from a stopped instance.
    pub fn reported_url(&self) -> Option<&str> {
        if self.running && self.reachable { self.browser_url.as_deref() } else { None }
    }

    /// Disabled mode, and any instance that is not running, makes no host-network requests.
    pub fn allows_external(&self) -> bool {
        self.running && self.active != HostMode::Disabled
    }
}

fn planned_addresses(mode: HostMode) -> Vec<String> {
    match mode {
        // The isolated station subnet. Shared has none until DHCP traffic says so.
        HostMode::User | HostMode::Setup => vec!["10.0.2.15".into()],
        HostMode::Shared | HostMode::Disabled => Vec::new(),
    }
}

/// Why shared mode cannot be combined with another host path. `None` means
/// the combination is not rejected here.
pub fn incompatible_shared(guest_ap: bool, setup_forward: bool, port_forward: bool, user_nat: bool) -> Option<&'static str> {
    if guest_ap {
        return Some("shared mode cannot carry a guest access point: 192.168.4.0/24 is not the vmnet shared subnet, and a per-service forward is not direct access");
    }
    if setup_forward {
        return Some("setup address forwards 192.168.4.1:80 on this Mac; it is not a vmnet shared guest address");
    }
    if port_forward {
        return Some("shared mode does not use a per-service TCP forward");
    }
    if user_nat {
        return Some("user-mode NAT is an isolated 10.0.2.0 network, not a vmnet shared adapter");
    }
    None
}

/// Permission text for a shared mode that is not up. A vmnet gateway that
/// overlaps the guest AP subnet replaces the status with that reason. The
/// gateway is never turned into a browser URL.
pub fn shared_failure_permission(code: &str, message: &str, gateway: Option<[u8; 4]>, mask: Option<[u8; 4]>) -> String {
    if let (Some(gateway), Some(mask)) = (gateway, mask) {
        if ap_subnet_overlaps(gateway, mask) {
            return format!(
                "vmnet gateway {} mask {} overlaps the guest AP subnet 192.168.4.0/24; shared mode is not offered",
                dotted(gateway),
                dotted(mask)
            );
        }
    }
    format!("{code}: {message}")
}

fn dotted(ip: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
}

/// `gateway` is the vmnet start address (the host side), not a guest.
/// True when that subnet collides with the guest AP `192.168.4.0/24`.
pub fn ap_subnet_overlaps(gateway: [u8; 4], mask: [u8; 4]) -> bool {
    if mask == [0, 0, 0, 0] {
        return true;
    }
    let ap = [192, 168, 4, 1];
    same_subnet(gateway, ap, mask) || same_subnet(gateway, ap, [255, 255, 255, 0])
}

fn same_subnet(a: [u8; 4], b: [u8; 4], mask: [u8; 4]) -> bool {
    let mask = u32::from_be_bytes(mask);
    (u32::from_be_bytes(a) & mask) == (u32::from_be_bytes(b) & mask)
}

fn scope(mode: HostMode) -> &'static str {
    match mode {
        HostMode::Disabled => "no host network access",
        HostMode::User => "this Mac, via a loopback forward",
        HostMode::Setup => "this Mac at http://192.168.4.1/ — not a Wi-Fi network, so a phone cannot join it",
        HostMode::Shared => "this Mac, at the guest address on the shared network",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_change_keeps_the_live_url_until_stop() {
        let mut info = NetworkInfo::stopped(HostMode::User);
        info.start(HostMode::User, Some("http://127.0.0.1:8080/".into()), vec!["127.0.0.1:8080".into()], "not-required");
        assert_eq!(info.reported_url(), Some("http://127.0.0.1:8080/"));
        info.configure(HostMode::Setup);
        assert_eq!(info.configured, HostMode::Setup);
        assert_eq!(info.active, HostMode::User);
        assert!(info.restart_required);
        assert_eq!(info.reported_url(), Some("http://127.0.0.1:8080/"));
        info.stop();
        assert!(info.reported_url().is_none());
        assert!(!info.reachable);
        assert!(!info.running);
        assert!(info.bindings.is_empty());
    }

    #[test]
    fn disabled_mode_makes_no_external_request_and_reports_discovery() {
        let mut info = NetworkInfo::stopped(HostMode::Disabled);
        info.start(HostMode::Disabled, None, Vec::new(), "not-required");
        assert!(!info.allows_external());
        assert!(info.reported_url().is_none());
        assert_eq!(info.discovery, "unsupported");
        assert!(!info.reachable);
    }

    #[test]
    fn shared_permission_failure_is_inactive_and_has_no_stale_address() {
        let mut info = NetworkInfo::stopped(HostMode::User);
        info.start(HostMode::User, Some("http://127.0.0.1:8080/".into()), vec!["127.0.0.1:8080".into()], "not-required");
        info.deny_shared("VmnetDenied: VMNET_FAILURE (1001)");
        assert_eq!(info.configured, HostMode::Shared);
        assert_eq!(info.active, HostMode::Disabled);
        assert!(!info.running);
        assert!(!info.reachable);
        assert!(info.reported_url().is_none());
        assert!(info.guest_addresses.is_empty());
        assert!(info.bindings.is_empty());
        let report = info.report();
        assert_eq!(report["permission"], "VmnetDenied: VMNET_FAILURE (1001)");
        assert_eq!(report["browser_url"], Value::Null);
        assert_eq!(report["reachable"], false);
        assert_eq!(report["guest_link"], "wifi-station");
        assert_eq!(report["active"], "disabled");
        assert_eq!(report["configured"], "shared");
        assert_eq!(report["restart_required"], false);
        assert_eq!(report["access_scope"], "shared mode is not active");
        assert!(report["guest_addresses"].as_array().unwrap().is_empty());
        assert!(report["bindings"].as_array().unwrap().is_empty());
        assert_eq!(report["discovery"], "unsupported");
    }

    /// The address is supplied by the test. It is not a vmnet lease.
    #[test]
    fn discovered_guest_address_is_not_a_live_vmnet() {
        let mut info = NetworkInfo::stopped(HostMode::Shared);
        assert!(info.guest_addresses.is_empty());
        assert!(info.reported_url().is_none());
        info.start(HostMode::Shared, None, Vec::new(), "authorized");
        assert!(!info.reachable);
        assert!(info.guest_addresses.is_empty());
        info.set_discovered_guest("203.0.113.10");
        assert_eq!(info.reported_url(), Some("http://203.0.113.10/"));
        assert_eq!(info.guest_addresses, vec!["203.0.113.10".to_string()]);
        assert!(info.bindings.is_empty());
        assert!(info.report()["reachable"].as_bool().unwrap());
        info.configure(HostMode::User);
        assert_eq!(info.active, HostMode::Shared);
        assert_eq!(info.configured, HostMode::User);
        assert!(info.restart_required);
        assert_eq!(info.reported_url(), Some("http://203.0.113.10/"));
        info.start(HostMode::User, Some("http://127.0.0.1:9/".into()), vec!["127.0.0.1:9".into()], "not-required");
        assert_eq!(info.reported_url(), Some("http://127.0.0.1:9/"));
        assert_eq!(info.active, HostMode::User);
        assert_eq!(info.guest_addresses, vec!["10.0.2.15".to_string()]);
        info.stop();
        assert!(info.reported_url().is_none());
        assert!(!info.reachable);
        assert!(info.guest_addresses.is_empty());
        assert!(info.bindings.is_empty());
        assert!(info.report()["browser_url"].is_null());
        assert_eq!(info.report()["reachable"], false);
        info.deny_shared("VmnetDenied: VMNET_FAILURE (1001)");
        info.set_discovered_guest("203.0.113.10");
        assert!(info.reported_url().is_none());
        assert!(info.guest_addresses.is_empty());
    }

    #[test]
    fn shared_rejects_ap_setup_forward_and_nat_combinations() {
        let ap = incompatible_shared(true, false, false, false).unwrap();
        assert!(ap.contains("192.168.4.0/24"));
        assert!(ap.contains("forward"));
        assert!(incompatible_shared(false, true, false, false).unwrap().contains("192.168.4.1"));
        assert!(incompatible_shared(false, false, true, false).unwrap().contains("per-service"));
        assert!(incompatible_shared(false, false, false, true).unwrap().contains("10.0.2.0"));
        assert!(incompatible_shared(false, false, false, false).is_none());
        assert!(ap_subnet_overlaps([192, 168, 4, 1], [255, 255, 255, 0]));
        assert!(ap_subnet_overlaps([192, 168, 4, 50], [255, 255, 255, 0]));
        assert!(!ap_subnet_overlaps([10, 0, 2, 2], [255, 255, 255, 0]));
        assert!(ap_subnet_overlaps([10, 0, 2, 2], [0, 0, 0, 0]));
        let overlapped = shared_failure_permission("VmnetInactive", "up", Some([192, 168, 4, 1]), Some([255, 255, 255, 0]));
        assert!(overlapped.contains("not offered"));
        assert!(!overlapped.contains("http://"));
        let denied = shared_failure_permission("VmnetDenied", "VMNET_FAILURE (1001)", None, None);
        assert_eq!(denied, "VmnetDenied: VMNET_FAILURE (1001)");
    }
}
