//! Bounded JSON operations. The helper accepts these and nothing that names a
//! program, a shell, or a path other than the caller's reconcile socket.

use crate::book::Code;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Lease,
    Heartbeat,
    Release,
    /// Ask vmnet for a shared interface. Not a TCP listener.
    Shared,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub op: Op,
    pub instance: String,
    pub nonce: String,
    pub pid: u32,
    pub control: Option<String>,
}

pub fn parse_request(line: &str) -> Result<Request, Code> {
    if line.len() > 4096 || !line.is_ascii() {
        return Err(Code::BadRequest);
    }
    let value: serde_json::Value = serde_json::from_str(line).map_err(|_| Code::BadRequest)?;
    let obj = value.as_object().ok_or(Code::BadRequest)?;
    let op = match obj.get("op").and_then(|v| v.as_str()).ok_or(Code::BadRequest)? {
        "lease" => Op::Lease,
        "heartbeat" => Op::Heartbeat,
        "release" => Op::Release,
        "shared" => Op::Shared,
        _ => return Err(Code::BadRequest),
    };
    let instance = obj.get("instance").and_then(|v| v.as_str()).ok_or(Code::BadRequest)?.to_string();
    let nonce = obj.get("nonce").and_then(|v| v.as_str()).ok_or(Code::BadRequest)?.to_string();
    let pid = obj.get("pid").and_then(|v| v.as_u64()).ok_or(Code::BadRequest)?;
    if pid == 0 || pid > u32::MAX as u64 || !is_uuid(&instance) || !is_nonce(&nonce) {
        return Err(Code::BadRequest);
    }
    let control = match obj.get("control").and_then(|v| v.as_str()) {
        Some(path) if valid_control(path) => Some(path.to_string()),
        Some(_) => return Err(Code::BadRequest),
        None if op == Op::Lease => return Err(Code::BadRequest),
        None => None,
    };
    Ok(Request { op, instance, nonce, pid: pid as u32, control })
}

fn is_uuid(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(i, c)| {
        if matches!(i, 8 | 13 | 18 | 23) { *c == b'-' } else { c.is_ascii_hexdigit() }
    })
}

fn is_nonce(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|c| c.is_ascii_hexdigit())
}

fn valid_control(path: &str) -> bool {
    path.len() < 100
        && path.starts_with('/')
        && !path.contains("..")
        && std::path::Path::new(path).file_name().and_then(|n| n.to_str()) == Some("helper.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(control: &str) -> String {
        format!(
            r#"{{"op":"lease","instance":"550e8400-e29b-41d4-a716-446655440000","nonce":"00112233445566778899aabbccddeeff","pid":42,"control":"{control}"}}"#
        )
    }

    #[test]
    fn a_well_formed_lease_parses_and_hostile_bytes_do_not_panic() {
        let ok = parse_request(&lease("/tmp/note/helper.sock")).unwrap();
        assert_eq!(ok.op, Op::Lease);
        assert_eq!(ok.pid, 42);
        assert!(parse_request(&lease("/tmp/../etc/helper.sock")).is_err());
        assert!(parse_request(&lease("helper.sock")).is_err());
        let shared = parse_request(
            r#"{"op":"shared","instance":"550e8400-e29b-41d4-a716-446655440000","nonce":"00112233445566778899aabbccddeeff","pid":42}"#,
        )
        .unwrap();
        assert_eq!(shared.op, Op::Shared);
        assert!(shared.control.is_none());
        assert!(parse_request(r#"{"op":"exec","instance":"550e8400-e29b-41d4-a716-446655440000","nonce":"00112233445566778899aabbccddeeff","pid":1}"#).is_err());
        assert!(parse_request("").is_err());
        assert!(parse_request("{").is_err());

        let mut state = 0x1234_5678_9abc_def0u64;
        for _ in 0..400 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let n = ((state >> 33) as usize) % 300;
            let bytes: Vec<u8> = (0..n).map(|i| ((state >> (i % 56)) & 0xff) as u8).collect();
            let line = String::from_utf8_lossy(&bytes);
            let _ = parse_request(&line);
        }
    }
}
