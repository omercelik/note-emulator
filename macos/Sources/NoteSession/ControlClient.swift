import Darwin
import Foundation
import NoteProtocol

public enum ControlError: Error, Equatable, CustomStringConvertible {
    case pathTooLong
    case socket(String)
    case closed
    case timeout
    case rejected(String)

    public var description: String {
        switch self {
        case .pathTooLong: "socket path is longer than the platform allows"
        case .socket(let message): message
        case .closed: "control connection closed"
        case .timeout: "control request timed out"
        case .rejected(let message): message
        }
    }
}

public struct ControlReply: Equatable, Sendable {
    public var json: String
    public var frames: [[UInt8]]
    public var events: [String]
    /// `Audio` payloads for this request (speaker PCM, little-endian i16 mono).
    public var audio: [[UInt8]] = []
}

public enum JSONField: Sendable {
    case string(String)
    case int(Int)
    case bool(Bool)
}

public func jsonObject(_ pairs: [(String, JSONField)]) -> String {
    let body = pairs.map { key, value in
        "\"\(jsonEscape(key))\":\(jsonValue(value))"
    }.joined(separator: ",")
    return "{\(body)}"
}

/// One protocol connection. Requests are serialized. A `display.get` reply
/// carries the frame that belongs to that request.
public final class ControlClient: @unchecked Sendable {
    // The state lock never covers I/O. Requests own a duplicate descriptor so shutdown can
    // interrupt them without closing a descriptor another thread might subsequently reuse.
    private var fd: Int32 = -1
    private let stateLock = NSLock()
    private var generation: UInt64 = 0
    private var decoder = Decoder()
    private var nextID: UInt64 = 1
    private let lock = NSLock()
    public var timeout: TimeInterval = 5

    public init() {}

    deinit { shutdown() }

    public func shutdown() {
        stateLock.withLock {
            generation &+= 1
            if fd >= 0 {
                Darwin.shutdown(fd, SHUT_RDWR)
                Darwin.close(fd)
                fd = -1
            }
        }
    }

    public func connect(_ path: String) throws {
        let ticket = stateLock.withLock { generation }
        lock.lock()
        defer { lock.unlock() }
        let bytes = Array(path.utf8)
        guard bytes.count < 104 else { throw ControlError.pathTooLong }
        let created = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard created >= 0 else { throw ControlError.socket(errnoText()) }
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        withUnsafeMutablePointer(to: &addr.sun_path) { raw in
            raw.withMemoryRebound(to: UInt8.self, capacity: 104) { dst in
                for (index, byte) in bytes.enumerated() { dst[index] = byte }
                dst[bytes.count] = 0
            }
        }
        let rc = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                Darwin.connect(created, sa, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard rc == 0 else {
            let message = errnoText()
            Darwin.close(created)
            throw ControlError.socket(message)
        }
        var tv = timeval(tv_sec: 0, tv_usec: 200_000)
        _ = setsockopt(created, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        _ = setsockopt(created, SOL_SOCKET, SO_SNDTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        var noSignal: Int32 = 1
        _ = setsockopt(created, SOL_SOCKET, SO_NOSIGPIPE, &noSignal, socklen_t(MemoryLayout<Int32>.size))
        try stateLock.withLock {
            guard generation == ticket else {
                Darwin.close(created)
                throw ControlError.closed
            }
            if fd >= 0 { Darwin.close(fd) }
            fd = created
            decoder = Decoder()
        }
    }

    public func request(_ pairs: [(String, JSONField)]) throws -> ControlReply {
        try request(json: Array(jsonObject(pairs).utf8))
    }

    public func request(json payload: [UInt8]) throws -> ControlReply {
        lock.lock()
        defer { lock.unlock() }
        let socket = try duplicateSocket()
        defer { Darwin.close(socket) }
        let id = nextID
        nextID &+= 1
        let encoded = try Message(kind: .request, requestID: id, payload: payload).encoded()
        let deadline = Date().addingTimeInterval(timeout)
        do {
            try writeAll(encoded, socket: socket, deadline: deadline)
        } catch {
            // A partial envelope cannot be followed by another request on this stream.
            shutdown()
            throw error
        }
        var frames: [[UInt8]] = []
        var events: [String] = []
        var audio: [[UInt8]] = []
        var pending: String?
        while Date() < deadline {
            if let message = try decoder.next() {
                switch message.kind {
                case .response where message.requestID == id:
                    let json = String(decoding: message.payload, as: UTF8.self)
                    if replyFailed(json) { throw ControlError.rejected(json) }
                    pending = json
                case .frame where message.requestID == id || message.requestID == 0:
                    frames.append(message.payload)
                case .audio where message.requestID == id:
                    audio.append(message.payload)
                case .event:
                    events.append(String(decoding: message.payload, as: UTF8.self))
                default:
                    break
                }
                if let json = pending, !json.contains("\"full\"") || !frames.isEmpty {
                    return ControlReply(json: json, frames: frames, events: events, audio: audio)
                }
                continue
            }
            _ = try readMore(socket)
        }
        throw ControlError.timeout
    }

    /// For a connection that sent `display.subscribe`: frames the runtime pushed (request id 0),
    /// waiting up to the socket's receive timeout (200 ms) for more bytes.
    public func pushedFrames() throws -> [[UInt8]] {
        lock.lock()
        defer { lock.unlock() }
        let socket = try duplicateSocket()
        defer { Darwin.close(socket) }
        _ = try readMore(socket)
        var frames: [[UInt8]] = []
        while let message = try decoder.next() {
            if message.kind == .frame && message.requestID == 0 { frames.append(message.payload) }
        }
        return frames
    }

    private func duplicateSocket() throws -> Int32 {
        try stateLock.withLock {
            guard fd >= 0 else { throw ControlError.closed }
            let copy = Darwin.dup(fd)
            guard copy >= 0 else { throw ControlError.socket(errnoText()) }
            return copy
        }
    }

    private func readMore(_ socket: Int32) throws -> Bool {
        var buf = [UInt8](repeating: 0, count: 64 * 1024)
        let count = Darwin.read(socket, &buf, buf.count)
        if count > 0 {
            decoder.feed(buf.prefix(count))
            return true
        }
        if count == 0 { throw ControlError.closed }
        if errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR { return false }
        throw ControlError.socket(errnoText())
    }

    private func writeAll(_ bytes: [UInt8], socket: Int32, deadline: Date) throws {
        var sent = 0
        while sent < bytes.count {
            guard Date() < deadline else { throw ControlError.timeout }
            let count = bytes.withUnsafeBytes { raw -> Int in
                guard let base = raw.baseAddress else { return -1 }
                return Darwin.write(socket, base.advanced(by: sent), bytes.count - sent)
            }
            if count > 0 {
                sent += count
                continue
            }
            if count < 0 && (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK) { continue }
            throw ControlError.closed
        }
    }
}

public func jsonInt(_ object: [String: Any], _ key: String) -> Int? {
    if let number = object[key] as? Int { return number }
    if let number = object[key] as? NSNumber { return number.intValue }
    return nil
}

public func jsonBool(_ object: [String: Any], _ key: String) -> Bool {
    if let flag = object[key] as? Bool { return flag }
    if let number = object[key] as? NSNumber { return number.boolValue }
    return false
}

public func jsonValue(_ object: Any?, _ key: String) -> String {
    guard let object = object as? [String: Any] else { return "" }
    if let text = object[key] as? String { return text }
    if object[key] is NSNull { return "" }
    if let number = object[key] as? NSNumber { return number.stringValue }
    return ""
}

public func jsonObject(_ text: String) -> [String: Any] {
    guard let data = text.data(using: .utf8),
          let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
    else { return [:] }
    return object
}

private func replyFailed(_ json: String) -> Bool {
    guard let data = json.data(using: .utf8),
          let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
          let ok = object["ok"] as? Bool
    else { return false }
    return !ok
}

private func jsonValue(_ field: JSONField) -> String {
    switch field {
    case .string(let text): "\"\(jsonEscape(text))\""
    case .int(let number): String(number)
    case .bool(let flag): flag ? "true" : "false"
    }
}

private func jsonEscape(_ text: String) -> String {
    var out = ""
    for scalar in text.unicodeScalars {
        switch scalar {
        case "\\": out += "\\\\"
        case "\"": out += "\\\""
        case "\n": out += "\\n"
        case "\r": out += "\\r"
        default: out.unicodeScalars.append(scalar)
        }
    }
    return out
}

private func errnoText() -> String {
    String(cString: strerror(errno))
}
