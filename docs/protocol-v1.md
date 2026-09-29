# NOTE local protocol v1 (draft)

Transport: per-user Unix-domain stream socket, `run/<instance-uuid>/control.sock` under the data
home (a short `$TMPDIR` path is used when the path would exceed the `sun_path` limit; the
instance descriptor records the real path).

## Envelope (implemented: `crates/note-protocol`, `macos/Sources/NoteProtocol`)

| Offset | Size | Field | Encoding |
|---|---|---|---|
| 0 | 4 | magic | ASCII `NOTE` |
| 4 | 2 | version | u16 LE, = 1 |
| 6 | 2 | kind | u16 LE: 1 Request, 2 Response, 3 Event, 16 Frame, 17 Audio, 18 Chunk |
| 8 | 8 | request_id | u64 LE; 0 for unsolicited events/frames |
| 16 | 4 | payload_length | u32 LE |
| 20 | n | payload | JSON (UTF-8) for kinds 1–3; binary for 16–18 |

Limits: 256 KiB control, 8 MiB binary. Bad magic, unknown version/kind, oversize length and
non-UTF-8 control payloads are fatal for the connection. Shared fixture:
`crates/note-protocol/fixtures/hello-request.bin` (generated independently in Python).

## Handshake

Request `{"method":"hello","protocol":1,"client":"ndb/0.1.0"}` →
Response `{"ok":true,"protocol":1,"runtime":"…","engine":"esp32sim@4ab7e90+note.N",
"profile":{"id":"note4","revision":"pcb-1.0"},"avd":"<uuid>","instance":"<uuid>","epoch":1,
"capabilities":["display.gray4","buttons","battery.live",…]}`.

## Requests (Spec §12.2)

`hello, status, pause, resume, reset, stop · button.down, button.up, button.press ·
battery.get, battery.set, power.get, power.set · clock.get, clock.set · network.info,
network.configure · display.get, screenshot.capture · console.write, logs.export ·
snapshot.list/save/load/delete · record.start/stop · settings.get, diagnose · boot.download,
boot.normal`. Every response: `{"ok":true,…}` or `{"ok":false,"error":{"code":"…","message":"…"}}`.

## Frame payload (kind 16) — frozen in G3

64-byte little-endian header (`crates/note-protocol/src/frame.rs`) followed by pixels.
Stride is not a field: pal2 is 4 pixels per byte and gray4 is 2, so the row size is
fixed by the width. Packing is high bits first; gray4 high nibble is the left pixel.

| Offset | Type | Field |
|---|---|---|
| 0 | u64 | instance_hash |
| 8 | u32 | epoch |
| 12 | u64 | seq |
| 20 | u64 | base_seq (`0` = full image; otherwise the seq the receiver must already hold) |
| 28 | u64 | virtual_ns |
| 36 | u64 | host_ns |
| 44 | u16 | width |
| 46 | u16 | height |
| 48 | u8 | format (`1` pal2, `2` gray4) |
| 49 | u8 | palette_id |
| 50 | u8 | source (`1` panel, `2` legacy-console) |
| 51 | u8 | refresh (`1` full, `2` partial, `3` gray) |
| 52 | u16×4 | dirty x, y, w, h |
| 60 | u32 | pixel_hash (FNV-1a of the payload) |

A client applies a delta only when `base_seq` equals the sequence it holds and the epoch
matches. Anything else is rejected and the next message must be a full image. A full image
(`base_seq` 0) is always applied, also in a new epoch: that is how a client follows a restart
or a snapshot restore (`snapshot.load` starts a new epoch).
