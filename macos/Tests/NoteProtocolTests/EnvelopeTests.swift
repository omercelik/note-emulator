import Foundation
import Testing
@testable import NoteProtocol

@Suite struct EnvelopeTests {
    @Test func helloRequestMatchesSharedFixture() throws {
        let url = try #require(Bundle.module.url(forResource: "hello-request", withExtension: "bin", subdirectory: "Fixtures"))
        let fixture = [UInt8](try Data(contentsOf: url))
        let message = Message(kind: .request, requestID: 1, payload: Array(#"{"method":"hello","protocol":1}"#.utf8))
        #expect(try message.encoded() == fixture)

        var decoder = Decoder()
        decoder.feed(fixture)
        #expect(try decoder.next() == message)
    }

    @Test func byteAtATimeStream() throws {
        let a = try Message(kind: .request, requestID: 7, payload: Array("{}".utf8)).encoded()
        let b = try Message(kind: .frame, requestID: 8, payload: [UInt8](repeating: 0xAB, count: 60_000)).encoded()
        var decoder = Decoder()
        var got: [(MessageKind, UInt64, Int)] = []
        for byte in a + b {
            decoder.feed([byte])
            while let m = try decoder.next() { got.append((m.kind, m.requestID, m.payload.count)) }
        }
        #expect(got.map(\.0) == [.request, .frame])
        #expect(got.map(\.2) == [2, 60_000])
    }

    @Test func hostileHeadersAreRejected() throws {
        var header = try Message(kind: .request, requestID: 1, payload: []).encoded()
        header.replaceSubrange(16..<20, with: [0xFF, 0xFF, 0xFF, 0xFF])
        var decoder = Decoder()
        decoder.feed(header)
        #expect(throws: EnvelopeError.tooLarge(kind: .request, length: .max)) { try decoder.next() }

        var http = Decoder()
        http.feed(Array("HTTP".utf8))
        #expect(throws: EnvelopeError.badMagic) { try http.next() }
    }
}
