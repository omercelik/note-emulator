import Darwin
import Foundation
import Testing
@testable import NoteSession

private final class StalledControl {
    let path = "/tmp/note-cancel-\(UUID().uuidString).sock"
    let listener: Int32
    init() throws {
        listener = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        withUnsafeMutablePointer(to: &addr.sun_path) { ptr in
            ptr.withMemoryRebound(to: UInt8.self, capacity: 104) { dst in
                for (i, byte) in path.utf8.enumerated() { dst[i] = byte }
            }
        }
        let rc = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(listener, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        try #require(rc == 0 && Darwin.listen(listener, 1) == 0)
    }
    deinit { Darwin.close(listener); Darwin.unlink(path) }
}

struct ControlCancellationTests {
    @Test func shutdownInterruptsARequestWaitingForAReply() throws {
        let server = try StalledControl()
        let client = ControlClient()
        defer { client.shutdown() }
        try client.connect(server.path)
        let peer = Darwin.accept(server.listener, nil, nil)
        defer { Darwin.close(peer) }
        let finished = DispatchSemaphore(value: 0)
        DispatchQueue.global().async {
            defer { finished.signal() }
            _ = try? client.request([("method", .string("status"))])
        }
        var bytes = [UInt8](repeating: 0, count: 1024)
        try #require(Darwin.read(peer, &bytes, bytes.count) > 0)
        let start = Date()
        client.shutdown()
        #expect(Date().timeIntervalSince(start) < 1)
        #expect(finished.wait(timeout: .now() + 1) == .success)
        // Reusing the client after cancellation must have a fresh decoder/socket.
        try client.connect(server.path)
        let replacement = Darwin.accept(server.listener, nil, nil)
        defer { Darwin.close(replacement) }
        client.shutdown()
    }

    @Test func stalledWritesRespectTheRequestDeadline() throws {
        let server = try StalledControl()
        let client = ControlClient()
        client.timeout = 0.3
        defer { client.shutdown() }
        try client.connect(server.path)
        let peer = Darwin.accept(server.listener, nil, nil)
        defer { Darwin.close(peer) }
        // The peer never drains its socket. The maximum control payload exceeds its buffer.
        let start = Date()
        do {
            _ = try client.request(json: [UInt8](repeating: 32, count: 256 * 1024))
            Issue.record("stalled peer unexpectedly replied")
        } catch {
            #expect(error as? ControlError == .timeout)
        }
        #expect(Date().timeIntervalSince(start) < 1.5)
    }
}
