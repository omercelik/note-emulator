import Foundation

/// NOTE local protocol v1 wire envelope — the Swift twin of
/// `crates/note-protocol`. Both are tested against the same fixture bytes.
public enum NoteProtocol {
    public static let magic: [UInt8] = Array("NOTE".utf8)
    public static let version: UInt16 = 1
    public static let headerLength = 20
    public static let maxControlPayload: UInt32 = 256 * 1024
    public static let maxBinaryPayload: UInt32 = 8 * 1024 * 1024
}

public enum MessageKind: UInt16, Sendable {
    case request = 1, response = 2, event = 3
    case frame = 16, audio = 17, chunk = 18

    public var isBinary: Bool { self == .frame || self == .audio || self == .chunk }
    public var maxPayload: UInt32 { isBinary ? NoteProtocol.maxBinaryPayload : NoteProtocol.maxControlPayload }
}

public struct Message: Equatable, Sendable {
    public var kind: MessageKind
    public var requestID: UInt64
    public var payload: [UInt8]

    public init(kind: MessageKind, requestID: UInt64, payload: [UInt8]) {
        self.kind = kind
        self.requestID = requestID
        self.payload = payload
    }

    public func encoded() throws(EnvelopeError) -> [UInt8] {
        guard payload.count <= Int(kind.maxPayload) else {
            throw .tooLarge(kind: kind, length: UInt32(clamping: payload.count))
        }
        var out = NoteProtocol.magic
        out.reserveCapacity(NoteProtocol.headerLength + payload.count)
        out.appendLittleEndian(NoteProtocol.version)
        out.appendLittleEndian(kind.rawValue)
        out.appendLittleEndian(requestID)
        out.appendLittleEndian(UInt32(payload.count))
        out += payload
        return out
    }
}

public enum EnvelopeError: Error, Equatable {
    case badMagic
    case unsupportedVersion(UInt16)
    case unknownKind(UInt16)
    case tooLarge(kind: MessageKind, length: UInt32)
    case notUTF8
}

/// Incremental stream decoder; any error is fatal for the connection.
public struct Decoder: Sendable {
    private var buffer: [UInt8] = []

    public init() {}

    public mutating func feed(_ bytes: some Sequence<UInt8>) { buffer += bytes }

    public mutating func next() throws(EnvelopeError) -> Message? {
        let visible = min(buffer.count, 4)
        guard Array(buffer.prefix(visible)) == Array(NoteProtocol.magic.prefix(visible)) else { throw .badMagic }
        guard buffer.count >= NoteProtocol.headerLength else { return nil }
        let version: UInt16 = buffer.readLittleEndian(at: 4)
        guard version == NoteProtocol.version else { throw .unsupportedVersion(version) }
        let rawKind: UInt16 = buffer.readLittleEndian(at: 6)
        guard let kind = MessageKind(rawValue: rawKind) else { throw .unknownKind(rawKind) }
        let requestID: UInt64 = buffer.readLittleEndian(at: 8)
        let length: UInt32 = buffer.readLittleEndian(at: 16)
        guard length <= kind.maxPayload else { throw .tooLarge(kind: kind, length: length) }
        let total = NoteProtocol.headerLength + Int(length)
        guard buffer.count >= total else { return nil }
        let payload = Array(buffer[NoteProtocol.headerLength..<total])
        buffer.removeFirst(total)
        if !kind.isBinary, String(validating: payload, as: UTF8.self) == nil { throw .notUTF8 }
        return Message(kind: kind, requestID: requestID, payload: payload)
    }
}

extension Array where Element == UInt8 {
    mutating func appendLittleEndian<T: FixedWidthInteger>(_ value: T) {
        Swift.withUnsafeBytes(of: value.littleEndian) { append(contentsOf: $0) }
    }

    func readLittleEndian<T: FixedWidthInteger>(at offset: Int) -> T {
        var value: T = 0
        for i in 0..<MemoryLayout<T>.size { value |= T(self[offset + i]) << (8 * i) }
        return value
    }
}
