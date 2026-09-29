#!/usr/bin/env python3
"""Extract boot firmware from a supplied NOTE4C dump, dropping all saved device data."""
import hashlib
import pathlib
import struct
import sys

source, destination = map(pathlib.Path, sys.argv[1:])
data = source.read_bytes()
if len(data) != 16 * 1024 * 1024:
    raise SystemExit("NOTE4C factory dump must be 16 MB")
partitions = []
for pos in range(0x8000, 0x9000, 32):
    magic, kind, subtype, offset, size, label, flags = struct.unpack("<HBBII16sI", data[pos:pos + 32])
    if magic != 0x50AA:
        break
    if offset + size > len(data):
        raise SystemExit("Partition extends beyond flash")
    partitions.append((kind, subtype, offset, size))
apps = [p for p in partitions if p[:2] == (0, 0x10)]
if len(apps) != 1:
    raise SystemExit("Expected one ota_0 application partition")

def image(offset, limit):
    header = data[offset:offset + 24]
    if header[0] != 0xE9 or not 1 <= header[1] <= 16:
        raise SystemExit("Invalid ESP firmware image")
    cursor = offset + 24
    for _ in range(header[1]):
        _, length = struct.unpack("<II", data[cursor:cursor + 8])
        cursor += 8 + length
        if cursor >= offset + limit:
            raise SystemExit("Firmware segment exceeds image bounds")
    end = offset + ((cursor - offset + 16) // 16) * 16
    if header[23]:
        if hashlib.sha256(data[offset:end]).digest() != data[end:end + 32]:
            raise SystemExit("Firmware image SHA-256 mismatch")
        end += 32
    if end > offset + limit:
        raise SystemExit("Firmware image exceeds partition")
    return data[offset:end]

# Keep only the bootloader, partition table, and first application. All NVS, OTA
# selection, calibration, secondary firmware, assets, gaps, and trailing bytes are erased.
clean = bytearray(b"\xff" * len(data))
bootloader = image(0, 0x8000)
clean[:len(bootloader)] = bootloader
clean[0x8000:0x9000] = data[0x8000:0x9000]
_, _, offset, size = apps[0]
application = image(offset, size)
clean[offset:offset + len(application)] = application
destination.parent.mkdir(parents=True, exist_ok=True)
destination.write_bytes(clean)
print("Prepared NOTE4C factory sample with saved device data erased")
