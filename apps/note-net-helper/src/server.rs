//! Lease server. One thread per caller, so a heartbeat connection does not
//! block the next device. Disconnect does not release a lease.

use std::fs;
use std::io::{self, Write};
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::book::{code_str, Book, Code, Endpoint, Lease, Verdict};
use crate::passfd::{read_line, send_with_fd};
use crate::platform::Platform;
use crate::proto::{parse_request, Op, Request};

#[derive(serde::Serialize)]
struct Journal<'a> {
    leases: &'a [Lease],
}

pub struct Helper<P> {
    pub platform: P,
    pub book: Book,
    journal: PathBuf,
    /// The helper's own reference to each granted listener, by lease instance. Closing the
    /// sender's copy while the descriptor is still in flight lets the kernel's unix-socket
    /// garbage collector tear the socket down (seen on macOS: every later connection is
    /// aborted). Keeping it for the lease's life avoids that; the helper never accepts on it.
    held: std::collections::HashMap<String, TcpListener>,
}

enum Outcome {
    Reply(String),
    Granted { json: String, listener: TcpListener, instance: String },
}

impl<P: Platform> Helper<P> {
    pub fn open(platform: P, journal: PathBuf) -> io::Result<Helper<P>> {
        let book = load_book(&journal)?;
        Ok(Helper { platform, book, journal, held: Default::default() })
    }

    pub fn startup_reconcile(&mut self) {
        let leases = self.book.leases.clone();
        let mut keep = Vec::new();
        for lease in leases {
            if !self.platform.pid_alive(lease.pid) {
                if lease.alias_owned {
                    let _ = self.platform.remove_alias(lease.endpoint);
                }
                continue;
            }
            match ask_holding(&lease) {
                Ok(true) => {
                    let mut live = lease;
                    live.holding = true;
                    keep.push(live);
                }
                Ok(false) => {
                    if lease.alias_owned {
                        let _ = self.platform.remove_alias(lease.endpoint);
                    }
                }
                Err(_) => keep.push(lease), // alive, no answer yet: do not give the endpoint away
            }
        }
        self.book.leases = keep;
        let _ = self.save();
    }

    fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.journal.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&Journal { leases: &self.book.leases })
            .map_err(|e| io::Error::other(e))?;
        let tmp = self.journal.with_extension("json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &self.journal)?;
        Ok(())
    }

    fn reap(&mut self) {
        let dead = self.book.reap(|pid| self.platform.pid_alive(pid));
        for lease in dead {
            if lease.alias_owned {
                let _ = self.platform.remove_alias(lease.endpoint);
            }
        }
    }

    fn dispatch(&mut self, req: Request) -> Outcome {
        self.reap();
        let outcome = match req.op {
            Op::Lease => self.lease(req),
            Op::Heartbeat => {
                if self.book.heartbeat(&req.instance, &req.nonce) {
                    Outcome::Reply(ok_json(req.endpoint, true))
                } else {
                    Outcome::Reply(err_json(Code::BadRequest, "unknown lease", None))
                }
            }
            Op::Shared => self.shared(),
            Op::Version => Outcome::Reply(format!("{{\"ok\":true,\"protocol\":{}}}\n", crate::PROTOCOL)),
            Op::Release => match self.book.release(&req.instance, &req.nonce) {
                Ok(lease) => {
                    if lease.alias_owned {
                        let _ = self.platform.remove_alias(lease.endpoint);
                    }
                    Outcome::Reply(ok_json(lease.endpoint, false))
                }
                Err(code) => Outcome::Reply(err_json(code, "unknown lease", None)),
            },
        };
        self.drop_released_listeners();
        let _ = self.save();
        outcome
    }

    /// Close the helper's copy of every listener whose lease is gone.
    fn drop_released_listeners(&mut self) {
        let leases = &self.book.leases;
        self.held.retain(|instance, _| leases.iter().any(|l| &l.instance == instance));
    }

    fn keep_listener(&mut self, instance: String, listener: TcpListener) {
        if self.book.leases.iter().any(|l| l.instance == instance) {
            self.held.insert(instance, listener);
        }
    }

    /// Shared mode is a vmnet interface, not the setup listener. The reply
    /// never carries an fd. `vmnet_gateway` is the interface start address,
    /// not a guest.
    fn shared(&mut self) -> Outcome {
        let probe = crate::vmnet::probe_shared();
        let (code, gateway) = match &probe {
            crate::vmnet::SharedProbe::Failed { .. } => (Code::VmnetDenied, None),
            crate::vmnet::SharedProbe::NoGuest { gateway, mask, .. } => {
                (Code::VmnetInactive, Some((gateway.clone(), mask.clone())))
            }
        };
        Outcome::Reply(shared_json(code, &probe.inactive_message(), gateway))
    }

    fn lease(&mut self, req: Request) -> Outcome {
        let control = req.control.clone().unwrap_or_default();
        let endpoint = req.endpoint;
        match self.book.request(&self.platform.view(), &req.instance, endpoint) {
            Verdict::Deny { code, owner, message } => Outcome::Reply(err_json(code, &message, owner.as_deref())),
            Verdict::AlreadyHeld => Outcome::Reply(ok_json(endpoint, true)),
            Verdict::Grant { own_alias } => {
                if own_alias {
                    if let Err(err) = self.platform.add_alias(endpoint) {
                        return Outcome::Reply(err_json(Code::HelperUnavailable, &err, None));
                    }
                }
                match self.platform.bind(endpoint) {
                    Ok(listener) => {
                        let instance = req.instance.clone();
                        self.book.insert(Lease {
                            instance: req.instance, nonce: req.nonce, pid: req.pid, control,
                            alias_owned: own_alias, holding: true, endpoint,
                        });
                        Outcome::Granted { json: ok_json(endpoint, false), listener, instance }
                    }
                    Err(err) => {
                        if own_alias {
                            let _ = self.platform.remove_alias(endpoint);
                        }
                        let code = if err.to_ascii_lowercase().contains("in use") || err.to_ascii_lowercase().contains("occupied") {
                            Code::EndpointOccupied
                        } else {
                            Code::HelperUnavailable
                        };
                        Outcome::Reply(err_json(code, &err, None))
                    }
                }
            }
        }
    }
}

fn load_book(path: &Path) -> io::Result<Book> {
    if !path.exists() {
        return Ok(Book::default());
    }
    let text = fs::read_to_string(path)?;
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| serde_json::from_value::<Vec<Lease>>(v.get("leases")?.clone()).ok())
        .map(|leases| Book { leases })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("cannot read {}", path.display())))
}

fn ok_json(endpoint: Endpoint, already: bool) -> String {
    format!("{{\"ok\":true,\"endpoint\":\"{}:80\",\"already\":{already}}}\n", endpoint.ip_text())
}

fn shared_json(code: Code, message: &str, gateway: Option<(String, String)>) -> String {
    let extra = gateway
        .map(|(gw, mask)| format!(",\"vmnet_gateway\":\"{}\",\"vmnet_mask\":\"{}\"", json_escape(&gw), json_escape(&mask)))
        .unwrap_or_default();
    format!("{{\"ok\":false,\"error\":\"{}\",\"message\":\"{}\"{extra}}}\n", code_str(code), json_escape(message))
}

fn err_json(code: Code, message: &str, owner: Option<&str>) -> String {
    let owner = owner.map(|o| format!(",\"owner\":\"{o}\"")).unwrap_or_default();
    format!("{{\"ok\":false,\"error\":\"{}\",\"message\":\"{}\"{owner}}}\n", code_str(code), json_escape(message))
}

fn json_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn ask_holding(lease: &Lease) -> io::Result<bool> {
    let mut sock = UnixStream::connect(&lease.control)?;
    sock.set_read_timeout(Some(Duration::from_secs(1)))?;
    sock.set_write_timeout(Some(Duration::from_secs(1)))?;
    let line = format!("{{\"op\":\"reconcile\",\"instance\":\"{}\",\"nonce\":\"{}\"}}\n", lease.instance, lease.nonce);
    (&sock).write_all(line.as_bytes())?;
    let reply = read_line(&mut sock, 4096)?;
    let value: serde_json::Value = serde_json::from_str(&reply).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(value.get("holding").and_then(|v| v.as_bool()).unwrap_or(true))
}

struct Peer {
    uid: u32,
    pid: u32,
}

fn peer_cred(sock: &UnixStream) -> io::Result<Peer> {
    let fd = sock.as_raw_fd();
    let mut uid = 0u32;
    let mut gid = 0u32;
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    if unsafe { libc::getsockopt(fd, libc::SOL_LOCAL, libc::LOCAL_PEERPID, &mut pid as *mut libc::pid_t as *mut libc::c_void, &mut len) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Peer { uid, pid: pid as u32 })
}

fn handle<P: Platform>(state: Arc<Mutex<Helper<P>>>, mut sock: UnixStream, stop: Arc<AtomicBool>, owner_uid: u32) {
    // Short reads so a stopped helper drops callers promptly. A timeout with
    // no bytes is not a disconnect and does not release the lease.
    // Accepted sockets inherit O_NONBLOCK from the listener on BSD; without this the read
    // timeout never applies and an idle caller spins a core on WouldBlock.
    let _ = sock.set_nonblocking(false);
    let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = sock.set_write_timeout(Some(Duration::from_secs(2)));
    let peer = match peer_cred(&sock) {
        Ok(peer) => peer,
        Err(_) => return,
    };
    if peer.uid != owner_uid {
        let _ = send_with_fd(&sock, err_json(Code::BadRequest, "peer user does not match", None).as_bytes(), None);
        return;
    }
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let line = match read_line(&mut sock, 4096) {
            Ok(line) => line,
            Err(err) if err.kind() == io::ErrorKind::TimedOut || err.kind() == io::ErrorKind::WouldBlock => continue,
            Err(_) => return,
        };
        let req = match parse_request(&line) {
            Ok(req) => req,
            Err(code) => {
                let _ = send_with_fd(&sock, err_json(code, "malformed request", None).as_bytes(), None);
                return;
            }
        };
        if req.pid != peer.pid {
            let _ = send_with_fd(&sock, err_json(Code::BadRequest, "pid does not match the socket peer", None).as_bytes(), None);
            return;
        }
        let outcome = state.lock().expect("helper").dispatch(req);
        match outcome {
            Outcome::Reply(json) => {
                if send_with_fd(&sock, json.as_bytes(), None).is_err() {
                    return;
                }
            }
            Outcome::Granted { json, listener, instance } => {
                let sent = send_with_fd(&sock, json.as_bytes(), Some(listener.as_raw_fd()));
                state.lock().expect("helper").keep_listener(instance, listener);
                if sent.is_err() {
                    return;
                }
            }
        }
    }
}

/// Accept callers until `stop` is set. Existing leases are left for the next
/// process to reconcile; stopping the helper is not a release.
pub fn serve<P: Platform + 'static>(listener: UnixListener, state: Arc<Mutex<Helper<P>>>, stop: Arc<AtomicBool>, owner_uid: u32) {
    let _ = listener.set_nonblocking(true);
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((sock, _)) => {
                let state = Arc::clone(&state);
                let stop = Arc::clone(&stop);
                thread::spawn(move || handle(state, sock, stop, owner_uid));
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(5)),
            // An aborted pending connection (ECONNABORTED) or a signal: keep serving leases.
            Err(err) if matches!(err.kind(), io::ErrorKind::ConnectionAborted | io::ErrorKind::Interrupted) => continue,
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::Auth;
    use crate::client::Session;
    use crate::platform::FakePlatform;
    use serde_json::Value;
    use std::time::Instant;

    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(format!("/tmp/note-helper-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct Running {
        stop: Arc<AtomicBool>,
        sock: PathBuf,
        state: Arc<Mutex<Helper<FakePlatform>>>,
        dir: PathBuf,
    }

    impl Running {
        fn start(name: &str, platform: FakePlatform) -> Running {
            Self::start_for_uid(name, platform, unsafe { libc::getuid() })
        }

        fn start_for_uid(name: &str, platform: FakePlatform, owner_uid: u32) -> Running {
            let dir = scratch(name);
            let sock = dir.join("helper.sock");
            let journal = dir.join("leases.json");
            let listener = UnixListener::bind(&sock).unwrap();
            let helper = Helper::open(platform, journal).unwrap();
            let state = Arc::new(Mutex::new(helper));
            let stop = Arc::new(AtomicBool::new(false));
            let shared = Arc::clone(&state);
            let flag = Arc::clone(&stop);
            thread::spawn(move || serve(listener, shared, flag, owner_uid));
            Running { stop, sock, state, dir }
        }

        fn stop(self) -> Arc<Mutex<Helper<FakePlatform>>> {
            self.stop.store(true, Ordering::Relaxed);
            self.state
        }
    }

    fn response_error(err: &crate::client::HelperError) -> String {
        err.code.clone()
    }

    #[test]
    fn only_the_configured_user_can_lease_the_endpoint() {
        let uid = unsafe { libc::getuid() };
        let rejected = Running::start_for_uid("other-user", FakePlatform::authorized(), uid + 1);
        let err = match Session::acquire(&rejected.sock, &rejected.dir.join("a/helper.sock")) {
            Ok(_) => panic!("wrong user received a listener"),
            Err(err) => err,
        };
        assert_eq!(err.code, "BadRequest");
        assert_eq!(rejected.state.lock().unwrap().platform.alias_adds, 0);
        rejected.stop();

        let accepted = Running::start_for_uid("configured-user", FakePlatform::authorized(), uid);
        let lease = Session::acquire(&accepted.sock, &accepted.dir.join("a/helper.sock")).unwrap();
        lease.release().unwrap();
        accepted.stop();
    }

    #[test]
    fn grant_passes_a_working_fd_and_a_second_owner_is_refused() {
        let running = Running::start("grant", FakePlatform::authorized());
        let mut first = Session::acquire(&running.sock, &running.dir.join("a/helper.sock")).unwrap();
        let listener = first.take_listener().unwrap();
        let port = listener.local_addr().unwrap().port();
        let _accepted = crate::passfd::accept_one(&listener, port);

        let Err(second) = Session::acquire(&running.sock, &running.dir.join("b/helper.sock")) else {
            panic!("second owner was granted the setup address");
        };
        assert_eq!(response_error(&second), "AddressInUse");
        assert_eq!(second.owner.as_deref(), Some(first.instance()));
        assert_eq!(running.state.lock().unwrap().platform.alias_adds, 1);

        first.release().unwrap();
        assert!(!running.state.lock().unwrap().platform.alias);
        let mut again = Session::acquire(&running.sock, &running.dir.join("c/helper.sock")).unwrap();
        assert!(again.take_listener().is_some());
        again.release().unwrap();
        running.stop();
    }

    #[test]
    fn setup_and_station_are_separate_leases_with_their_own_aliases() {
        let running = Running::start("station", FakePlatform::authorized());
        let mut setup = Session::acquire(&running.sock, &running.dir.join("a/helper.sock")).unwrap();
        let mut station = Session::acquire_endpoint(&running.sock, &running.dir.join("b/helper.sock"), Endpoint::Station).unwrap();
        assert!(setup.take_listener().is_some() && station.take_listener().is_some());
        {
            let helper = running.state.lock().unwrap();
            assert!(helper.platform.alias && helper.platform.station_alias);
            let endpoints: Vec<_> = helper.book.leases.iter().map(|l| l.endpoint).collect();
            assert_eq!(endpoints, [Endpoint::Setup, Endpoint::Station]);
        }
        let Err(second) = Session::acquire_endpoint(&running.sock, &running.dir.join("c/helper.sock"), Endpoint::Station) else {
            panic!("a second owner was granted the station address");
        };
        assert_eq!(second.code, "AddressInUse");

        station.release().unwrap();
        {
            let helper = running.state.lock().unwrap();
            assert!(helper.platform.alias && !helper.platform.station_alias, "only the station alias goes");
        }
        running.state.lock().unwrap().platform.alive.remove(&std::process::id());
        let _ = setup.heartbeat();
        assert!(!running.state.lock().unwrap().platform.alias, "a dead runtime's setup alias is removed");
        running.stop();
    }

    #[test]
    fn a_helper_from_before_endpoints_is_not_used_for_the_station() {
        // It ignores "endpoint" and grants 192.168.4.1:80; the client must not take that
        // listener as the station address.
        let dir = scratch("outdated");
        let sock = dir.join("helper.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let old = thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let _ = read_line(&mut conn, 4096).unwrap();
            let tcp = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            send_with_fd(&conn, b"{\"ok\":true,\"endpoint\":\"192.168.4.1:80\",\"already\":false}\n", Some(tcp.as_raw_fd())).unwrap();
            read_line(&mut conn, 4096).unwrap()
        });
        let err = match Session::acquire_endpoint(&sock, &dir.join("a/helper.sock"), Endpoint::Station) {
            Ok(_) => panic!("an outdated helper's setup lease was accepted as the station"),
            Err(err) => err,
        };
        assert_eq!(err.code, "HelperOutdated");
        assert!(old.join().unwrap().contains("\"op\":\"release\""), "the mistaken lease is handed back");
    }

    #[test]
    fn the_protocol_version_is_reported_and_an_older_helper_counts_as_one() {
        let running = Running::start("version", FakePlatform::authorized());
        assert_eq!(crate::client::installed_protocol(&running.sock), Some(crate::PROTOCOL));
        assert!(running.state.lock().unwrap().book.leases.is_empty(), "asking leases nothing");
        running.stop();

        // A helper from before `version` answers any unknown request with BadRequest.
        let dir = scratch("version-old");
        let sock = dir.join("helper.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let _ = read_line(&mut conn, 4096);
            let _ = send_with_fd(&conn, err_json(Code::BadRequest, "malformed request", None).as_bytes(), None);
        });
        assert_eq!(crate::client::installed_protocol(&sock), Some(1));
        assert_eq!(crate::client::installed_protocol(&dir.join("absent.sock")), None);
    }

    #[test]
    fn cancellation_conflict_and_bind_failure_do_not_keep_an_alias() {
        let mut denied = FakePlatform::authorized();
        denied.auth = Auth::Cancelled;
        let running = Running::start("cancel", denied);
        let Err(err) = Session::acquire(&running.sock, &running.dir.join("a/helper.sock")) else {
            panic!("cancelled authorization granted a lease");
        };
        assert_eq!(err.code, "AuthorizationCancelled");
        assert_eq!(running.state.lock().unwrap().platform.alias_adds, 0);
        running.stop();

        let mut conflict = FakePlatform::authorized();
        conflict.addrs.push(crate::book::Addr { ip: crate::book::SETUP_IP, prefix: 24, iface: "en0".into() });
        let running = Running::start("alias", conflict);
        let Err(err) = Session::acquire(&running.sock, &running.dir.join("a/helper.sock")) else {
            panic!("an address conflict granted a lease");
        };
        assert_eq!(err.code, "AliasConflict");
        assert_eq!(running.state.lock().unwrap().platform.alias_adds, 0);
        running.stop();

        let mut fail = FakePlatform::authorized();
        fail.bind_error = Some("address already in use".into());
        let running = Running::start("bind", fail);
        let Err(err) = Session::acquire(&running.sock, &running.dir.join("a/helper.sock")) else {
            panic!("a failed bind granted a lease");
        };
        assert_eq!(err.code, "EndpointOccupied");
        let (adds, removes, alias) = {
            let platform = &running.state.lock().unwrap().platform;
            (platform.alias_adds, platform.alias_removes, platform.alias)
        };
        assert_eq!(adds, 1);
        assert_eq!(removes, 1);
        assert!(!alias);
        running.stop();
    }

    #[test]
    fn a_dead_runtime_is_cleaned_up_and_a_live_one_survives_silence() {
        let running = Running::start("death", FakePlatform::authorized());
        let mut session = Session::acquire(&running.sock, &running.dir.join("a/helper.sock")).unwrap();
        assert!(running.state.lock().unwrap().platform.alias);
        running.state.lock().unwrap().platform.alive.remove(&std::process::id());
        // The next request reaps the dead pid. This process is that pid, so the
        // platform hook — not a missing heartbeat — is what counts as exit.
        let _ = session.heartbeat();
        let (alias, removes) = {
            let platform = &running.state.lock().unwrap().platform;
            (platform.alias, platform.alias_removes)
        };
        assert!(!alias);
        assert_eq!(removes, 1);
        running.stop();
    }

    #[test]
    fn helper_restart_reconciles_a_live_runtime_and_the_endpoint_can_be_reused() {
        let dir = scratch("restart");
        let sock = dir.join("helper.sock");
        let journal = dir.join("leases.json");
        let listener = UnixListener::bind(&sock).unwrap();
        let helper = Helper::open(FakePlatform::authorized(), journal.clone()).unwrap();
        let state = Arc::new(Mutex::new(helper));
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let shared = Arc::clone(&state);
        thread::spawn(move || serve(listener, shared, flag, unsafe { libc::getuid() }));

        let control = dir.join("run/helper.sock");
        let mut session = Session::acquire(&sock, &control).unwrap();
        let held = session.take_listener().unwrap();
        assert!(state.lock().unwrap().platform.alias);

        stop.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_secs(2);
        while session.heartbeat().is_ok() {
            assert!(Instant::now() < deadline, "helper connection stayed open after stop");
        }
        session.mark_closed();
        drop(held);
        let _ = fs::remove_file(&sock);

        let answered = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&answered);
        let mut session_thread = session;
        let polling = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline && !flag.load(Ordering::Relaxed) {
                if session_thread.poll_control().unwrap() {
                    flag.store(true, Ordering::Relaxed);
                }
                thread::sleep(Duration::from_millis(5));
            }
        });

        let mut restarted = Helper::open(FakePlatform::authorized(), journal).unwrap();
        // The new process does not share the fake alias bit. Replay the journal:
        // the runtime answers holding=false, so the alias must be removed.
        restarted.platform.alias = true;
        restarted.platform.alias_adds = 1;
        restarted.startup_reconcile();
        polling.join().unwrap();
        assert!(answered.load(Ordering::Relaxed));
        assert!(!restarted.platform.alias);
        assert!(restarted.book.leases.is_empty());
        assert_eq!(restarted.platform.alias_removes, 1);

        let listener = UnixListener::bind(&sock).unwrap();
        let state = Arc::new(Mutex::new(restarted));
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let shared = Arc::clone(&state);
        thread::spawn(move || serve(listener, shared, flag, unsafe { libc::getuid() }));
        let mut again = Session::acquire(&sock, &dir.join("run2/helper.sock")).unwrap();
        assert!(again.take_listener().is_some());
        again.release().unwrap();
        stop.store(true, Ordering::Relaxed);
    }

    #[test]
    fn status_shape_names_the_owner() {
        let running = Running::start("shape", FakePlatform::authorized());
        let err = Session::acquire(&running.sock, &running.dir.join("a/helper.sock"));
        let session = err.unwrap();
        let text = fs::read_to_string(running.dir.join("leases.json")).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["leases"][0]["instance"], session.instance());
        drop(session);
        running.stop();
    }

    /// Live `vmnet_start_interface` (shared mode) through the helper socket.
    /// Not a simulated LAN: the status is the one this process gets, and the
    /// reply must not grant an address or a listening fd.
    #[test]
    fn shared_request_returns_the_live_vmnet_status_and_no_guest_address() {
        let running = Running::start("shared", FakePlatform::authorized());
        let err = crate::client::request_shared(&running.sock);
        assert_eq!(err.code, "VmnetDenied", "{err}");
        assert_eq!(err.message, "VMNET_FAILURE (1001)");
        assert!(err.gateway.is_none());
        assert!(err.mask.is_none());
        assert!(!err.message.contains("192.168"));
        assert_eq!(running.state.lock().unwrap().platform.alias_adds, 0);
        assert!(running.state.lock().unwrap().book.leases.is_empty());
        running.stop();
    }
}
