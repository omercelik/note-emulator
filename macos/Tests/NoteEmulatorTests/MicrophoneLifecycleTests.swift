import Foundation
import Testing
@testable import NoteEmulator

@MainActor
private final class PermissionGate {
    var continuation: CheckedContinuation<Bool, Never>?
    func wait() async -> Bool {
        await withCheckedContinuation { continuation = $0 }
    }
    func entered() async throws {
        for _ in 0..<1000 {
            if continuation != nil { return }
            await Task.yield()
        }
        try #require(continuation != nil)
    }
    func finish() { continuation?.resume(returning: true); continuation = nil }
}

private final class FakeMicrophone: MicrophoneCapture {
    var level: Float = 0
    var chunk = Data()
    var starts = 0
    var stops = 0
    func start() { starts += 1 }
    func stop() { stops += 1 }
    func take() -> Data { chunk }
}

@MainActor
struct MicrophoneLifecycleTests {
    @Test func disablingWhilePermissionIsPendingNeverStartsCapture() async throws {
        let gate = PermissionGate()
        let mic = FakeMicrophone()
        let model = DeviceModel(socketPath: "unused", microphoneAccess: { await gate.wait() },
                                makeMicrophone: { mic }, connectMicrophone: { _, _ in })
        model.macMicrophone = true
        let pending = try #require(model.micTask)
        try await gate.entered()
        model.macMicrophone = false
        gate.finish()
        await pending.value
        #expect(mic.starts == 0)
        #expect(!model.macMicrophone)
    }

    @Test func detachWhileConnectingNeverStartsCapture() async throws {
        let gate = PermissionGate()
        let mic = FakeMicrophone()
        let model = DeviceModel(socketPath: "unused", microphoneAccess: { true },
                                makeMicrophone: { mic }, connectMicrophone: { _, _ in _ = await gate.wait() })
        model.macMicrophone = true
        let pending = try #require(model.micTask)
        try await gate.entered()
        model.detach()
        gate.finish()
        await pending.value
        #expect(mic.starts == 0)
        #expect(!model.macMicrophone)
    }

    @Test func transportFailureStopsCaptureAndClearsTheToggle() async throws {
        let mic = FakeMicrophone()
        mic.chunk = Data([0, 0])
        // Connecting is stubbed, so sending the first chunk produces ControlError.closed.
        let model = DeviceModel(socketPath: "unused", microphoneAccess: { true },
                                makeMicrophone: { mic }, connectMicrophone: { _, _ in })
        model.macMicrophone = true
        let task = try #require(model.micTask)
        await task.value
        #expect(mic.starts == 1)
        #expect(mic.stops > 0)
        #expect(!model.macMicrophone)
        #expect(model.lastError.contains("microphone:"))
    }

    @Test func cancelledAttemptCannotStopItsReplacement() async throws {
        let gate = PermissionGate()
        let mic = FakeMicrophone()
        var attempts = 0
        let model = DeviceModel(socketPath: "unused", microphoneAccess: { true }, makeMicrophone: { mic },
                                connectMicrophone: { _, _ in
                                    attempts += 1
                                    if attempts == 1 { _ = await gate.wait() }
                                })
        model.macMicrophone = true
        let old = try #require(model.micTask)
        try await gate.entered()
        model.macMicrophone = false
        model.macMicrophone = true
        let current = try #require(model.micTask)
        for _ in 0..<1000 {
            if mic.starts == 1 { break }
            await Task.yield()
        }
        gate.finish()
        await old.value
        #expect(model.macMicrophone)
        #expect(mic.starts == 1)
        #expect(mic.stops == 0)
        model.macMicrophone = false
        await current.value
        #expect(mic.stops > 0)
    }
}
