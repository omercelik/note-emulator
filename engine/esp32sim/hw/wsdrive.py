#!/usr/bin/env python3
"""Drive the emulator's web UI protocol without a browser and measure real-time keep-up.
usage: wsdrive.py [port] [seconds]  — presses btn1 / turns the knob every ~0.6 s, reports push gaps."""
import socket, struct, json, time, sys, os, base64, threading
port = int(sys.argv[1]) if len(sys.argv) > 1 else 8766
dur = float(sys.argv[2]) if len(sys.argv) > 2 else 12
s = socket.create_connection(("127.0.0.1", port))
key = base64.b64encode(os.urandom(16)).decode()
s.sendall(f"GET / HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n".encode())
buf = b""
while b"\r\n\r\n" not in buf: buf += s.recv(4096)
buf = buf.split(b"\r\n\r\n", 1)[1]
def send(obj):
    d = json.dumps(obj).encode(); m = os.urandom(4)
    hdr = bytes([0x81]) + (bytes([0x80 | len(d)]) if len(d) < 126 else bytes([0x80 | 126]) + struct.pack(">H", len(d)))
    s.sendall(hdr + m + bytes(b ^ m[i & 3] for i, b in enumerate(d)))
stats, audio, lock = [], [], threading.Lock()
def reader():
    global buf
    while True:
        while len(buf) < 2:
            r = s.recv(65536)
            if not r: return
            buf += r
        op = buf[0] & 0xf; ln = buf[1] & 0x7f; p = 2
        if ln == 126: ln = struct.unpack(">H", buf[2:4])[0]; p = 4
        elif ln == 127: ln = struct.unpack(">Q", buf[2:10])[0]; p = 10
        while len(buf) < p + ln:
            r = s.recv(65536)
            if not r: return
            buf += r
        data = buf[p:p + ln]; buf = buf[p + ln:]
        now = time.time()
        if op == 1:
            m = json.loads(data)
            if m.get("t") == "stat":
                with lock: stats.append((m["time"], m.get("behind", 0), now))
        elif op == 2 and data and data[0] == 2:
            with lock: audio.append(((len(data) - 5) // 2, now, struct.unpack('<I', data[1:5])[0]))
threading.Thread(target=reader, daemon=True).start()
t0 = time.time(); i = 0
while time.time() - t0 < dur:
    send({"t": "gpio", "pin": 17, "level": 0}); time.sleep(0.12); send({"t": "gpio", "pin": 17, "level": 1}); time.sleep(0.25)
    send({"t": "knob", "dir": "cw", "n": 1}); time.sleep(0.25); i += 1
time.sleep(1.0)
with lock:
    st = list(stats); au = list(audio)
gaps = [(st[k][2] - st[k-1][2]) * 1e3 for k in range(1, len(st))]
slow = [g for g in gaps if g > 40]
print(f"{i} press+knob cycles; {len(st)} stat pushes over {st[-1][2]-st[0][2]:.1f} s wall / {st[-1][0]-st[0][0]:.2f} s emulated")
print(f"push gaps: max {max(gaps):.0f} ms, {len(slow)} over 40 ms (sum {sum(slow):.0f} ms); max behind {max(x[1] for x in st):.2f} s")
sm = sum(n for n, _, _ in au); rate = au[-1][2] if au else 44100; print(f"audio: {len(au)} chunks, {sm} samples at {rate} Hz = {sm/rate:.2f} s")
