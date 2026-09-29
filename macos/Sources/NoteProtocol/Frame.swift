import Foundation

/// Canonical display frame. The layout matches `crates/note-protocol/src/frame.rs`:
/// a 64-byte little-endian header, then pixels. Stride is not stored.
public enum FrameFormat {
    public static let pal2: UInt8 = 1
    public static let gray4: UInt8 = 2
    public static let headerLength = 64
    /// NOTE4C palette, profile order: black, white, yellow, red.
    public static let note4c: [[UInt8]] = [
        [0x28, 0x28, 0x27, 255],
        [0xF7, 0xF7, 0xF4, 255],
        [0xF4, 0xD8, 0x15, 255],
        [0xAE, 0x34, 0x30, 255],
    ]
}

public struct FrameHeader: Equatable, Sendable {
    public var instanceHash: UInt64
    public var epoch: UInt32
    public var seq: UInt64
    public var baseSeq: UInt64
    public var virtualNs: UInt64
    public var hostNs: UInt64
    public var width: UInt16
    public var height: UInt16
    public var format: UInt8
    public var paletteID: UInt8
    public var source: UInt8
    public var refresh: UInt8
    public var dirtyX: UInt16
    public var dirtyY: UInt16
    public var dirtyW: UInt16
    public var dirtyH: UInt16
    public var pixelHash: UInt32

    public init(instanceHash: UInt64, epoch: UInt32, seq: UInt64, baseSeq: UInt64, virtualNs: UInt64, hostNs: UInt64, width: UInt16, height: UInt16, format: UInt8, paletteID: UInt8, source: UInt8, refresh: UInt8, dirtyX: UInt16, dirtyY: UInt16, dirtyW: UInt16, dirtyH: UInt16, pixelHash: UInt32) {
        self.instanceHash = instanceHash
        self.epoch = epoch
        self.seq = seq
        self.baseSeq = baseSeq
        self.virtualNs = virtualNs
        self.hostNs = hostNs
        self.width = width
        self.height = height
        self.format = format
        self.paletteID = paletteID
        self.source = source
        self.refresh = refresh
        self.dirtyX = dirtyX
        self.dirtyY = dirtyY
        self.dirtyW = dirtyW
        self.dirtyH = dirtyH
        self.pixelHash = pixelHash
    }
}

public enum FrameError: Error, Equatable {
    case short(Int)
    case format(UInt8)
    case length(got: Int, need: Int)
    case hash(got: UInt32, expected: UInt32)
    case unappliable(base: UInt64, held: UInt64)
    case epoch(got: UInt32, held: UInt32)
}

public enum DisplayFrame {
    public static func fnv1a(_ data: [UInt8]) -> UInt32 {
        var hash: UInt32 = 0x811C_9DC5
        for byte in data {
            hash ^= UInt32(byte)
            hash = hash &* 0x0100_0193
        }
        return hash
    }

    public static func payloadLength(width: UInt16, height: UInt16, format: UInt8) -> Int? {
        let pixels = Int(width) * Int(height)
        switch format {
        case FrameFormat.pal2 where pixels % 4 == 0: return pixels / 4
        case FrameFormat.gray4 where pixels % 2 == 0: return pixels / 2
        default: return nil
        }
    }

    /// High bits are the left pixel. pal2 is 4 pixels per byte; gray4 is 2.
    public static func pixelIndex(_ pixels: [UInt8], format: UInt8, at index: Int) -> UInt8 {
        if format == FrameFormat.pal2 {
            return (pixels[index / 4] >> (6 - 2 * (index % 4))) & 3
        }
        return (pixels[index / 2] >> (index % 2 == 0 ? 4 : 0)) & 0x0F
    }

    public static func rgba(pixels: [UInt8], format: UInt8, width: Int, height: Int) -> [UInt8] {
        var out = [UInt8]()
        out.reserveCapacity(width * height * 4)
        for i in 0..<(width * height) {
            let index = Int(pixelIndex(pixels, format: format, at: i))
            if format == FrameFormat.pal2 {
                let color = FrameFormat.note4c[min(index, 3)]
                out.append(contentsOf: color)
            } else {
                let level = UInt8(index) * 17
                out.append(contentsOf: [level, level, level, 255])
            }
        }
        return out
    }

    public static func decode(_ buf: [UInt8]) throws(FrameError) -> (FrameHeader, [UInt8]) {
        guard buf.count >= FrameFormat.headerLength else { throw .short(buf.count) }
        let header = FrameHeader(
            instanceHash: read(buf, 0),
            epoch: read(buf, 8),
            seq: read(buf, 12),
            baseSeq: read(buf, 20),
            virtualNs: read(buf, 28),
            hostNs: read(buf, 36),
            width: read(buf, 44),
            height: read(buf, 46),
            format: buf[48],
            paletteID: buf[49],
            source: buf[50],
            refresh: buf[51],
            dirtyX: read(buf, 52),
            dirtyY: read(buf, 54),
            dirtyW: read(buf, 56),
            dirtyH: read(buf, 58),
            pixelHash: read(buf, 60)
        )
        let pixels = Array(buf[FrameFormat.headerLength...])
        try check(header, pixels)
        let expected = fnv1a(pixels)
        guard header.pixelHash == expected else { throw .hash(got: header.pixelHash, expected: expected) }
        return (header, pixels)
    }

    public static func encode(_ header: FrameHeader, _ pixels: [UInt8]) throws(FrameError) -> [UInt8] {
        try check(header, pixels)
        let expected = fnv1a(pixels)
        guard header.pixelHash == expected else { throw .hash(got: header.pixelHash, expected: expected) }
        var out = [UInt8]()
        out.reserveCapacity(FrameFormat.headerLength + pixels.count)
        append(&out, header.instanceHash)
        append(&out, header.epoch)
        append(&out, header.seq)
        append(&out, header.baseSeq)
        append(&out, header.virtualNs)
        append(&out, header.hostNs)
        append(&out, header.width)
        append(&out, header.height)
        out.append(header.format)
        out.append(header.paletteID)
        out.append(header.source)
        out.append(header.refresh)
        append(&out, header.dirtyX)
        append(&out, header.dirtyY)
        append(&out, header.dirtyW)
        append(&out, header.dirtyH)
        append(&out, header.pixelHash)
        out.append(contentsOf: pixels)
        return out
    }

    /// A full frame (`baseSeq == 0`) replaces the image. A delta applies only
    /// when `baseSeq` is the sequence the client holds and the epoch matches.
    public static func apply(heldEpoch: UInt32, heldSeq: UInt64, header: FrameHeader, pixels: [UInt8]) throws(FrameError) -> [UInt8] {
        try check(header, pixels)
        // A new epoch (restart, snapshot restore) is entered with a full frame; only a delta
        // has to belong to the held epoch.
        if header.baseSeq != 0 && header.epoch != heldEpoch && heldSeq != 0 {
            throw .epoch(got: header.epoch, held: heldEpoch)
        }
        if header.baseSeq != 0 && header.baseSeq != heldSeq {
            throw .unappliable(base: header.baseSeq, held: heldSeq)
        }
        return pixels
    }

    private static func check(_ header: FrameHeader, _ pixels: [UInt8]) throws(FrameError) {
        guard let need = payloadLength(width: header.width, height: header.height, format: header.format) else {
            throw .format(header.format)
        }
        guard pixels.count == need else { throw .length(got: pixels.count, need: need) }
    }
}

/// Image the window is showing. The first frame may arrive with any epoch.
public struct PanelState: Equatable, Sendable {
    public private(set) var epoch: UInt32 = 0
    public private(set) var seq: UInt64 = 0
    public private(set) var width = 0
    public private(set) var height = 0
    public private(set) var format: UInt8 = 0
    public private(set) var pixels: [UInt8] = []
    /// Virtual time of the held frame's commit.
    public private(set) var virtualNs: UInt64 = 0

    public init() {}

    public mutating func ingest(_ bytes: [UInt8]) throws(FrameError) {
        let (header, pix) = try DisplayFrame.decode(bytes)
        let next = try DisplayFrame.apply(heldEpoch: epoch, heldSeq: seq, header: header, pixels: pix)
        epoch = header.epoch
        seq = header.seq
        width = Int(header.width)
        height = Int(header.height)
        format = header.format
        virtualNs = header.virtualNs
        pixels = next
    }

    public func rgba() -> [UInt8] {
        DisplayFrame.rgba(pixels: pixels, format: format, width: width, height: height)
    }
}

private func read<T: FixedWidthInteger>(_ buf: [UInt8], _ offset: Int) -> T {
    var value: T = 0
    for i in 0..<MemoryLayout<T>.size {
        value |= T(buf[offset + i]) << (8 * i)
    }
    return value
}

private func append<T: FixedWidthInteger>(_ out: inout [UInt8], _ value: T) {
    var rest = value
    for _ in 0..<MemoryLayout<T>.size {
        out.append(UInt8(rest & 0xFF))
        rest >>= 8
    }
}
