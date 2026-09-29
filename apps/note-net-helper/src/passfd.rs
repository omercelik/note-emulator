//! `SCM_RIGHTS` on a unix stream. One payload, at most one file descriptor.
//! The response line and the fd travel in the same `sendmsg`, so a plain
//! `read` cannot consume the byte and drop the descriptor.

use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

pub fn send_with_fd(sock: &UnixStream, payload: &[u8], fd: Option<RawFd>) -> io::Result<()> {
    let mut iov = libc::iovec { iov_base: payload.as_ptr() as *mut libc::c_void, iov_len: payload.len() };
    let mut cbuf = [std::mem::MaybeUninit::<libc::cmsghdr>::uninit(); 8];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    if fd.is_some() {
        msg.msg_control = cbuf.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = std::mem::size_of_val(&cbuf) as _;
        unsafe {
            let hdr = libc::CMSG_FIRSTHDR(&msg);
            if hdr.is_null() {
                return Err(io::Error::other("control buffer too small for one fd"));
            }
            (*hdr).cmsg_level = libc::SOL_SOCKET;
            (*hdr).cmsg_type = libc::SCM_RIGHTS;
            (*hdr).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as libc::c_uint) as _;
            std::ptr::write_unaligned(libc::CMSG_DATA(hdr) as *mut RawFd, fd.unwrap_or(-1));
            msg.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as libc::c_uint) as _;
        }
    }
    let n = unsafe { libc::sendmsg(sock.as_raw_fd(), &msg, 0) };
    if n < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

pub fn recv_with_fd(sock: &UnixStream) -> io::Result<(Vec<u8>, Option<RawFd>)> {
    let mut buf = vec![0u8; 8192];
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr() as *mut libc::c_void, iov_len: buf.len() };
    let mut cbuf = [std::mem::MaybeUninit::<libc::cmsghdr>::uninit(); 8];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cbuf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = std::mem::size_of_val(&cbuf) as _;
    let n = unsafe { libc::recvmsg(sock.as_raw_fd(), &mut msg, 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    if n == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "helper closed the connection"));
    }
    let mut passed = None;
    unsafe {
        let mut hdr = libc::CMSG_FIRSTHDR(&msg);
        while !hdr.is_null() {
            if (*hdr).cmsg_level == libc::SOL_SOCKET && (*hdr).cmsg_type == libc::SCM_RIGHTS {
                passed = Some(std::ptr::read_unaligned(libc::CMSG_DATA(hdr) as *const RawFd));
                break;
            }
            hdr = libc::CMSG_NXTHDR(&msg, hdr);
        }
    }
    buf.truncate(n as usize);
    Ok((buf, passed))
}

/// Test helper: connect to `port` and accept that connection on `listener`. macOS may abort a
/// queued connection before `accept` (ECONNABORTED), notably while descriptors are in flight
/// over unix sockets; like any correct client, try again with a fresh connection.
#[cfg(test)]
pub fn accept_one(listener: &std::net::TcpListener, port: u16) -> std::net::TcpStream {
    for _ in 0..20 {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)));
        });
        match listener.accept() {
            Ok((peer, _)) => {
                assert!(rx.recv_timeout(std::time::Duration::from_secs(1)).is_ok_and(|c| c.is_ok()));
                return peer;
            }
            Err(e) if e.kind() == io::ErrorKind::ConnectionAborted => continue,
            Err(e) => panic!("accept: {e}"),
        }
    }
    panic!("every connection was aborted before accept");
}

pub fn read_line(sock: &mut UnixStream, max: usize) -> io::Result<String> {
    let mut out = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match sock.read(&mut byte) {
            Ok(0) => {
                return if out.is_empty() {
                    Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed"))
                } else {
                    Err(io::Error::new(io::ErrorKind::InvalidData, "request was not a single line"))
                };
            }
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) if out.len() >= max => return Err(io::Error::new(io::ErrorKind::InvalidData, "request exceeds 4096 bytes")),
            Ok(_) => out.push(byte[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    String::from_utf8(out).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "request is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, TcpListener};
    use std::os::fd::FromRawFd;

    #[test]
    fn a_passed_listener_still_accepts_after_the_sender_drops_it() {
        let (a, b) = UnixStream::pair().unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        send_with_fd(&a, b"hello\n", Some(listener.as_raw_fd())).unwrap();
        drop(listener);
        let (buf, fd) = recv_with_fd(&b).unwrap();
        assert_eq!(buf, b"hello\n");
        let listener = unsafe { TcpListener::from_raw_fd(fd.expect("fd")) };
        let _peer = accept_one(&listener, port);
    }
}
