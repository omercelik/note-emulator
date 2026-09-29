#!/usr/bin/env python3
"""Listen to the emulator UI's audio stream (binary type 2) for N seconds and report activity.
usage: wsaudio.py [port] [seconds]"""
import socket, struct, json, time, sys, os, base64
port = int(sys.argv[1]) if len(sys.argv) > 1 else 8766
dur = float(sys.argv[2]) if len(sys.argv) > 2 else 5
s = socket.create_connection(("127.0.0.1", port))
key = base64.b64encode(os.urandom(16)).decode()
s.sendall(f"GET / HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n".encode())
buf = b""
while b"\r\n\r\n" not in buf: buf += s.recv(4096)
buf = buf.split(b"\r\n\r\n", 1)[1]
s.settimeout(0.5); end = time.time() + dur; samples = 0; nonzero = 0; peak = 0
while time.time() < end:
    while len(buf) < 2:
        try: r = s.recv(65536)
        except socket.timeout: r = b""
        if not r: break
        buf += r
    if len(buf) < 2: continue
    op = buf[0] & 0xf; ln = buf[1] & 0x7f; p = 2
    if ln == 126: ln = struct.unpack(">H", buf[2:4])[0]; p = 4
    elif ln == 127: ln = struct.unpack(">Q", buf[2:10])[0]; p = 10
    while len(buf) < p + ln:
        try: r = s.recv(65536)
        except socket.timeout: r = b""
        if not r: break
        buf += r
    data = buf[p:p + ln]; buf = buf[p + ln:]
    if op == 2 and data and data[0] == 2:
        rate = struct.unpack("<I", data[1:5])[0]
        vals = struct.unpack("<%dh" % ((len(data) - 5) // 2), data[5:5 + ((len(data) - 5) // 2) * 2])
        samples += len(vals); nz = sum(1 for v in vals if v); nonzero += nz; peak = max(peak, max((abs(v) for v in vals), default=0))
print(f"{dur:.0f} s: {samples} samples at {rate} Hz, {nonzero} non-zero, peak {peak}")
