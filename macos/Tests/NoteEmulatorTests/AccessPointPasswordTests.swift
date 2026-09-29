import Foundation
import Testing
@testable import NoteEmulator

@MainActor
struct AccessPointPasswordTests {
    @Test func reopeningKeepsTheAppliedPasswordButANewRuntimeStartsEmpty() {
        let socket = "/tmp/test-\(UUID())/control.sock"
        AccessPointPasswordCache.remember("test-password", for: socket)
        let reopened = DeviceModel(socketPath: socket)
        #expect(reopened.setupHotspotPassword == "test-password")
        #expect(reopened.appliedSetupHotspotPassword == "test-password")
        let restarted = DeviceModel(socketPath: "/tmp/test-\(UUID())/control.sock")
        #expect(restarted.setupHotspotPassword.isEmpty)
        #expect(restarted.appliedSetupHotspotPassword.isEmpty)
    }
}
