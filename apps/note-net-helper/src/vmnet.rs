//! One call to Apple's vmnet shared-mode packet interface.
//!
//! The result is whatever `vmnet_start_interface` returns on this process.
//! A failure carries the status name and number. A success is stopped again:
//! no guest frame was read, so there is no guest address to report. The
//! start address vmnet returns is the gateway, not the guest.

use std::sync::Mutex;

static PROBE: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SharedProbe {
    Failed { status: String },
    /// `gateway` is `vmnet_start_address_key` (the host side of the subnet).
    NoGuest { status: String, gateway: String, mask: String, mac: String },
}

impl SharedProbe {
    pub fn code(&self) -> &'static str {
        match self {
            SharedProbe::Failed { .. } => "VmnetDenied",
            SharedProbe::NoGuest { .. } => "VmnetInactive",
        }
    }

    /// Text for `network.info`'s permission field. Never a guest URL.
    pub fn inactive_message(&self) -> String {
        match self {
            SharedProbe::Failed { status } => status.clone(),
            SharedProbe::NoGuest { status, gateway, mask, .. } => {
                if gateway.is_empty() {
                    format!("{status}; no guest address was learned from traffic; the interface was stopped")
                } else {
                    format!("{status}; no guest address was learned from traffic; the interface was stopped (vmnet gateway {gateway}, mask {mask})")
                }
            }
        }
    }
}

pub fn status_name(code: u32) -> &'static str {
    match code {
        1000 => "VMNET_SUCCESS",
        1001 => "VMNET_FAILURE",
        1002 => "VMNET_MEM_FAILURE",
        1003 => "VMNET_INVALID_ARGUMENT",
        1004 => "VMNET_SETUP_INCOMPLETE",
        1005 => "VMNET_INVALID_ACCESS",
        1006 => "VMNET_PACKET_TOO_BIG",
        1007 => "VMNET_BUFFER_EXHAUSTED",
        1008 => "VMNET_TOO_MANY_PACKETS",
        1009 => "VMNET_SHARING_SERVICE_BUSY",
        1010 => "VMNET_NOT_AUTHORIZED",
        _ => "VMNET_UNKNOWN",
    }
}

pub fn status_text(code: u32) -> String {
    format!("{} ({code})", status_name(code))
}

extern "C" {
    fn note_vmnet_probe_shared(
        mac: *mut libc::c_char,
        mac_len: usize,
        start_addr: *mut libc::c_char,
        start_len: usize,
        end_addr: *mut libc::c_char,
        end_len: usize,
        mask: *mut libc::c_char,
        mask_len: usize,
        mtu: *mut u64,
    ) -> u32;
}

fn c_buf() -> Vec<u8> {
    vec![0u8; 128]
}

fn c_str(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Call `vmnet_start_interface` in shared mode. Serialized: vmnet does not
/// document concurrent starts from one process.
pub fn probe_shared() -> SharedProbe {
    let _hold = PROBE.lock().unwrap_or_else(|e| e.into_inner());
    let mut mac = c_buf();
    let mut start = c_buf();
    let mut end = c_buf();
    let mut mask = c_buf();
    let mut mtu = 0u64;
    let code = unsafe {
        note_vmnet_probe_shared(
            mac.as_mut_ptr() as *mut libc::c_char,
            mac.len(),
            start.as_mut_ptr() as *mut libc::c_char,
            start.len(),
            end.as_mut_ptr() as *mut libc::c_char,
            end.len(),
            mask.as_mut_ptr() as *mut libc::c_char,
            mask.len(),
            &mut mtu,
        )
    };
    let status = status_text(code);
    if code == 1000 {
        SharedProbe::NoGuest { status, gateway: c_str(&start), mask: c_str(&mask), mac: c_str(&mac) }
    } else {
        let _ = (c_str(&end), mtu);
        SharedProbe::Failed { status }
    }
}
