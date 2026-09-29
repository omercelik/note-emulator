import Foundation
import Testing
@testable import NoteEmulator

private final class SetupPageProtocol: URLProtocol, @unchecked Sendable {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        guard let url = request.url else { return }
        if url.path == "/unreachable" {
            client?.urlProtocol(self, didFailWithError: URLError(.timedOut))
            return
        }
        let code = url.path == "/rejected" ? 403 : 200
        let response = HTTPURLResponse(url: url, statusCode: code, httpVersion: "HTTP/1.1", headerFields: nil)!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data("setup page".utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

struct AccessPointConnectionTests {
    @Test func connectionRequiresARespondingSetupPage() async {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [SetupPageProtocol.self]
        let session = URLSession(configuration: config)
        defer { session.invalidateAndCancel() }
        let reachable = await AccessPointReachability.check(URL(string: "http://192.168.4.1/")!, session: session)
        let rejected = await AccessPointReachability.check(URL(string: "http://192.168.4.1/rejected")!, session: session)
        let unreachable = await AccessPointReachability.check(URL(string: "http://192.168.4.1/unreachable")!, session: session)
        #expect(reachable == .reachable)
        #expect(rejected == .httpError(403))
        #expect(unreachable == .unreachable)
    }
}
