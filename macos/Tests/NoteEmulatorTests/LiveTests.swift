import AVFoundation
import AppKit
import SwiftUI
import Foundation
import NoteProtocol
import NoteSession
import Testing
@testable import NoteEmulator

/// Pixel hash of the NOTE4 demo main menu (v1.0.0), the same value as `firmware.rs`.
private let note4MenuHash: UInt32 = 3_467_383_573

@MainActor
@Suite(.serialized) struct DeviceWindowLiveTests {
    @Test func note4MenuButtonsBatteryLogAndALateSecondWindow() async throws {
        guard ProcessInfo.processInfo.environment["NOTE_G4_LIVE"] == "1" else { return }
        let repo = repoRoot()
        let running = try await boot(
            repo: repo,
            profile: "note4",
            firmware: repo.appendingPathComponent("third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin"),
            name: "g4-note4",
            extra: ["--forward", "127.0.0.1:0:80"]
        )
        defer { running.stop() }
        let model = running.model
        try await waitUntil("NOTE4 menu", seconds: 60) {
            model.panel.format == FrameFormat.gray4
                && model.panel.pixels.count == 60_000
                && DisplayFrame.fnv1a(model.panel.pixels) == note4MenuHash
        }
        #expect(model.engine == "esp32sim")
        #expect(model.profile == "note4")
        #expect(!model.lines.isEmpty)
        #expect(model.browserURL.hasPrefix("http://127.0.0.1:"))
        #expect(model.networkActive == "user")

        await model.press("down", down: true)
        #expect(model.heldButtons.contains("down"))
        try await waitUntil("down changes the menu", seconds: 30) {
            DisplayFrame.fnv1a(model.panel.pixels) != note4MenuHash
        }
        await model.releaseFocus()
        #expect(model.heldButtons.isEmpty)

        model.batteryMV = 3500
        await model.applyBattery()
        try await waitUntil("battery reports 3500 mV", seconds: 10) { model.reportedMV == 3500 }
        #expect(!model.usbCable && model.usbHost, "starts on battery with a debug host")
        await model.setUSB(cable: true)
        try await waitUntil("status reports the cable", seconds: 10) { model.usbCable }
        await model.setUSB(cable: false, host: false)
        #expect(!model.usbCable && !model.usbHost)

        let late = DeviceModel(socketPath: running.socket)
        late.start()
        try await waitUntil("late window has a full frame", seconds: 15) {
            late.image != nil && late.panel.width == 400 && late.panel.height == 300
                && late.panel.format == FrameFormat.gray4 && late.panel.pixels.count == 60_000
                && late.lastError.isEmpty
        }
        await model.stop()
    }

    /// G4 skin + CAP-01 through the window's model: NOTE4 loads its shell and power LED, and an
    /// MP4 recorded across a menu move holds both committed states on virtual time.
    @Test func note4SkinLedAndRecordingAcrossAMenuMove() async throws {
        guard ProcessInfo.processInfo.environment["NOTE_G4_LIVE"] == "1" else { return }
        let repo = repoRoot()
        let running = try await boot(
            repo: repo,
            profile: "note4",
            firmware: repo.appendingPathComponent("third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin"),
            name: "g4-skin",
            extra: []
        )
        defer { running.stop() }
        let model = running.model
        try await waitUntil("NOTE4 menu", seconds: 60) { DisplayFrame.fnv1a(model.panel.pixels) == note4MenuHash }
        let skin = try #require(model.skin)
        #expect(skin.button(at: CGPoint(x: 890, y: 891)) == "ok")
        // Screenshot: a real PNG of the menu, not an empty file.
        let shot = FileManager.default.temporaryDirectory.appendingPathComponent("shot-\(UUID()).png")
        defer { try? FileManager.default.removeItem(at: shot) }
        try model.saveScreenshot(to: shot)
        let data = try Data(contentsOf: shot)
        let rep = try #require(NSBitmapImageRep(data: data))
        #expect(rep.pixelsWide == 400 && rep.pixelsHigh == 300, "\(rep.pixelsWide)x\(rep.pixelsHigh), \(data.count) bytes")
        var dark = 0
        for y in stride(from: 0, to: 300, by: 3) { for x in stride(from: 0, to: 400, by: 3) {
            if let c = rep.colorAt(x: x, y: y), c.alphaComponent > 0.5, c.brightnessComponent < 0.5 { dark += 1 }
        }}
        #expect(dark > 500, "menu ink in the screenshot: \(dark) dark samples, \(data.count) bytes")
        if let path = ProcessInfo.processInfo.environment["NOTE_WINDOW_PNG"] {
            let renderer = ImageRenderer(content: HStack(alignment: .top, spacing: 30) {
                DeviceFrameContent(model: model).frame(width: 460, height: 464)
                DeviceFrameContent(model: model).overlay { BezelControls(model: model) }.frame(width: 460, height: 464)
                // TabView cannot be drawn off-screen; the controls are checked in the real window.
            }.padding(20).background(Color.white))
            renderer.scale = 1
            if let image = renderer.cgImage {
                try NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: path))
            }
        }
        try await waitUntil("status reports the power LED", seconds: 10) { model.leds["power"] != nil }

        let url = FileManager.default.temporaryDirectory.appendingPathComponent("g4-\(UUID()).mp4")
        defer { try? FileManager.default.removeItem(at: url) }
        model.startRecording(to: url)
        #expect(model.recording)
        let start = model.virtualSeconds
        await model.press("down", down: true)
        await model.press("down", down: false)
        try await waitUntil("down changes the menu", seconds: 30) { DisplayFrame.fnv1a(model.panel.pixels) != note4MenuHash }
        try await waitUntil("a second of virtual time", seconds: 30) { model.virtualSeconds > start + 1.5 }
        await model.stopRecording()
        #expect(!model.recording && model.lastError.isEmpty, "\(model.lastError)")
        let asset = AVURLAsset(url: url)
        let duration = try await asset.load(.duration).seconds
        #expect(duration > 1.0, "duration \(duration)")
        let track = try #require(try await asset.loadTracks(withMediaType: .video).first)
        let size = try await track.load(.naturalSize)
        #expect(size == CGSize(width: 800, height: 600))
        await model.stop()
    }

    /// G7 through the window's model: save at the menu, move the cursor, load, and the menu's
    /// exact pixels come back (a new frame epoch forces a full frame).
    @Test func note4SnapshotSaveMoveAndLoadRestoresTheMenu() async throws {
        guard ProcessInfo.processInfo.environment["NOTE_G4_LIVE"] == "1" else { return }
        let repo = repoRoot()
        let running = try await boot(
            repo: repo,
            profile: "note4",
            firmware: repo.appendingPathComponent("third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin"),
            name: "g7-snap",
            extra: []
        )
        defer { running.stop() }
        let model = running.model
        try await waitUntil("NOTE4 menu", seconds: 60) { DisplayFrame.fnv1a(model.panel.pixels) == note4MenuHash }
        try await waitUntil("pushed frames arrive", seconds: 10) { model.pushedFrames > 0 }
        await model.saveSnapshot("menu")
        #expect(model.snapshots.contains("menu"), "\(model.snapshotStatus)")
        await model.press("down", down: true)
        await model.press("down", down: false)
        try await waitUntil("down changes the menu", seconds: 30) { DisplayFrame.fnv1a(model.panel.pixels) != note4MenuHash }
        await model.loadSnapshot("menu")
        #expect(model.snapshotStatus == "Loaded menu", "\(model.snapshotStatus)")
        try await waitUntil("the menu is back", seconds: 10) { DisplayFrame.fnv1a(model.panel.pixels) == note4MenuHash }
        #expect(DisplayFrame.fnv1a(model.panel.pixels) == note4MenuHash,
                "hash \(DisplayFrame.fnv1a(model.panel.pixels)) epoch \(model.panel.epoch) seq \(model.panel.seq) err \(model.lastError) t \(model.virtualSeconds)")
        await model.stop()
    }

    @Test func note4cFactoryDrawsAFourColourFrame() async throws {
        guard ProcessInfo.processInfo.environment["NOTE_G4_LIVE"] == "1" else { return }
        // Private firmware: NOTE_PRIVATE_DIR, default .tools/private in the repo (gitignored).
        let privateDir = ProcessInfo.processInfo.environment["NOTE_PRIVATE_DIR"].map { URL(fileURLWithPath: $0) }
            ?? repoRoot().appendingPathComponent(".tools/private")
        let firmware = privateDir.appendingPathComponent(".device-backup/factory-2026-09-23.bin")
        let running = try await boot(
            repo: repoRoot(),
            profile: "note4c",
            firmware: firmware,
            name: "g4-factory",
            extra: []
        )
        defer { running.stop() }
        let model = running.model
        try await waitUntil("factory screen uses more than one colour", seconds: 120) {
            model.profile == "note4c"
                && model.panel.format == FrameFormat.pal2
                && model.panel.pixels.count == 30_000
                && paletteUsed(model.panel.pixels) >= 3
                && !model.lines.isEmpty
        }
        model.batteryMV = 4150
        await model.applyBattery()
        try await waitUntil("factory battery reports 4150 mV", seconds: 10) { model.reportedMV == 4150 }
        // AUDIO-01 host side: the factory audio test drives the speaker; its PCM reaches the window.
        model.speakerMuted = true
        try await waitUntil("speaker PCM from the factory audio test", seconds: 120) { model.speakerSamples > 1000 }
        #expect(model.networkActive == "disabled")
        #expect(model.browserURL.isEmpty)
        await model.stop()
    }
}

private struct RunningDevice {
    var process: Process
    var home: URL
    var socket: String
    var model: DeviceModel

    func stop() {
        if process.isRunning { process.terminate() }
        try? FileManager.default.removeItem(at: home)
    }
}

@MainActor
private func boot(repo: URL, profile: String, firmware: URL, name: String, extra: [String]) async throws -> RunningDevice {
    let ndb = repo.appendingPathComponent("target/release/ndb")
    let emu = repo.appendingPathComponent("target/release/note-emu")
    try #require(FileManager.default.isExecutableFile(atPath: ndb.path), "build release ndb first")
    try #require(FileManager.default.isExecutableFile(atPath: emu.path), "build release note-emu first")
    try #require(FileManager.default.fileExists(atPath: firmware.path))
    let home = URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
        .appendingPathComponent("note-g4-\(name)-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
    let rom = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Application Support/NOTE Emulator/rom")
    try FileManager.default.createSymbolicLink(at: home.appendingPathComponent("rom"), withDestinationURL: rom)
    var env = ProcessInfo.processInfo.environment
    env["NOTE_EMU_HOME"] = home.path
    let created = try run(ndb, ["avd", "create", "--profile", profile, "--firmware", firmware.path, "--name", name], env)
    let id = created.trimmingCharacters(in: .whitespacesAndNewlines)
    let process = Process()
    process.executableURL = emu
    process.arguments = ["--avd", id, "--seconds", "0"] + extra
    process.environment = env
    process.standardOutput = FileHandle.nullDevice
    process.standardError = FileHandle.nullDevice
    try process.run()
    var sock = ""
    for _ in 0..<100 {
        if let record = DataHome.listInstances(home).first(where: { $0.avd == id }) {
            sock = record.sock
            break
        }
        if !process.isRunning {
            throw ControlError.socket("note-emu exited before publishing a socket")
        }
        try await Task.sleep(for: .milliseconds(100))
    }
    try #require(!sock.isEmpty)
    let model = DeviceModel(socketPath: sock)
    model.start()
    return RunningDevice(process: process, home: home, socket: sock, model: model)
}

@MainActor
private func waitUntil(_ label: String, seconds: Int, _ ready: () -> Bool) async throws {
    for _ in 0..<(seconds * 10) {
        if ready() { return }
        try await Task.sleep(for: .milliseconds(100))
    }
    Issue.record("timed out waiting for \(label)")
}

private func paletteUsed(_ pixels: [UInt8]) -> Int {
    var seen = Set<UInt8>()
    let count = pixels.count * 4
    for index in 0..<count {
        seen.insert(DisplayFrame.pixelIndex(pixels, format: FrameFormat.pal2, at: index))
        if seen.count == 4 { break }
    }
    return seen.count
}

private func repoRoot() -> URL {
    URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
}

private func run(_ tool: URL, _ arguments: [String], _ environment: [String: String]) throws -> String {
    let process = Process()
    process.executableURL = tool
    process.arguments = arguments
    process.environment = environment
    let out = Pipe()
    let err = Pipe()
    process.standardOutput = out
    process.standardError = err
    try process.run()
    process.waitUntilExit()
    let text = String(data: out.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
    let errors = String(data: err.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
    try #require(process.terminationStatus == 0, "\(tool.lastPathComponent) failed: \(errors) \(text)")
    return text
}
