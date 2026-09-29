import Darwin
import Foundation
import NoteProtocol
import Testing
@testable import NoteSession

@Suite struct FrameAndLatchTests {
    @Test func gray4HighNibbleIsTheLeftPixelAndABadDeltaIsRejected() throws {
        let pixels: [UInt8] = [0x10, 0x32, 0x54, 0x76]
        let header = FrameHeader(
            instanceHash: 1, epoch: 1, seq: 2, baseSeq: 0, virtualNs: 3, hostNs: 4,
            width: 4, height: 2, format: FrameFormat.gray4, paletteID: 0, source: 1, refresh: 1,
            dirtyX: 0, dirtyY: 0, dirtyW: 4, dirtyH: 2, pixelHash: DisplayFrame.fnv1a(pixels)
        )
        let bytes = try DisplayFrame.encode(header, pixels)
        #expect(bytes.count == 68)
        #expect(bytes[12] == 2)
        #expect(bytes[48] == FrameFormat.gray4)
        var panel = PanelState()
        try panel.ingest(bytes)
        #expect(panel.seq == 2)
        #expect(DisplayFrame.pixelIndex(panel.pixels, format: FrameFormat.gray4, at: 0) == 1)
        #expect(DisplayFrame.pixelIndex(panel.pixels, format: FrameFormat.gray4, at: 1) == 0)
        let rgba = panel.rgba()
        #expect(rgba[0] == 17 && rgba[4] == 0)

        var delta = header
        delta.baseSeq = 9
        delta.seq = 3
        let other: [UInt8] = [0xFF, 0xFF, 0xFF, 0xFF]
        delta.pixelHash = DisplayFrame.fnv1a(other)
        let rejected = try DisplayFrame.encode(delta, other)
        #expect(throws: FrameError.unappliable(base: 9, held: 2)) { try panel.ingest(rejected) }
        #expect(panel.pixels == pixels)
    }

    @Test func pal2PacksFourPixelsWithTheProfileColors() {
        let pixels: [UInt8] = [0b11_10_01_00]
        #expect(DisplayFrame.pixelIndex(pixels, format: FrameFormat.pal2, at: 0) == 3)
        #expect(DisplayFrame.pixelIndex(pixels, format: FrameFormat.pal2, at: 3) == 0)
        let rgba = DisplayFrame.rgba(pixels: pixels, format: FrameFormat.pal2, width: 4, height: 1)
        #expect(Array(rgba[0..<3]) == [0xAE, 0x34, 0x30])
        #expect(Array(rgba[12..<15]) == [0x28, 0x28, 0x27])
    }

    @Test func focusLossReleasesEveryHeldButtonOnce() {
        var latch = ButtonLatch()
        let first = latch.down("up")
        let repeatDown = latch.down("up")
        let second = latch.down("ok")
        #expect(first)
        #expect(!repeatDown)
        #expect(second)
        #expect(latch.releaseAll() == ["ok", "up"])
        #expect(latch.releaseAll().isEmpty)
        let stray = latch.up("up")
        #expect(!stray)
    }
}

@Suite struct RegistryTests {
    @Test func listsAvdsAndSkipsADeadInstance() throws {
        let root = URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
            .appendingPathComponent("note-g4-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root.appendingPathComponent("avd/demo.avd"), withIntermediateDirectories: true)
        let config = #"{"id":"demo","name":"Demo","profile":"note4","source":"x","sha256":"ab"}"#
        try config.write(to: root.appendingPathComponent("avd/demo.avd/config.json"), atomically: true, encoding: .utf8)
        try FileManager.default.createDirectory(at: root.appendingPathComponent("run/alive"), withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: root.appendingPathComponent("run/dead"), withIntermediateDirectories: true)
        let alive = #"{"instance":"alive","avd":"demo","pid":\#(ProcessInfo.processInfo.processIdentifier),"sock":"/tmp/note-alive.sock"}"#
        let dead = #"{"instance":"dead","avd":"demo","pid":2147483000,"sock":"/tmp/note-dead.sock"}"#
        try alive.write(to: root.appendingPathComponent("run/alive/instance.json"), atomically: true, encoding: .utf8)
        try dead.write(to: root.appendingPathComponent("run/dead/instance.json"), atomically: true, encoding: .utf8)

        #expect(DataHome.listAvds(root).map(\.profile) == ["note4"])
        let running = DataHome.listInstances(root)
        #expect(running.map(\.instance) == ["alive"])
        #expect(!DataHome.processAlive(0))
        try? FileManager.default.removeItem(at: root)
    }
}

@Suite struct TwoClientTests {
    @Test func lateClientsBothReceiveAFullFrame() throws {
        let server = try MockControl()
        defer { server.stop() }
        let first = ControlClient()
        let second = ControlClient()
        defer { first.shutdown(); second.shutdown() }
        try first.connect(server.path)
        try second.connect(server.path)
        let a = try first.request([("method", .string("hello")), ("protocol", .int(1))])
        let b = try second.request([("method", .string("hello")), ("protocol", .int(1))])
        #expect(jsonValue(jsonObject(a.json), "engine") == "mock")
        #expect(jsonValue(jsonObject(b.json), "engine") == "mock")

        let frameA = try first.request([("method", .string("display.get"))])
        let frameB = try second.request([("method", .string("display.get"))])
        var panelA = PanelState()
        var panelB = PanelState()
        try panelA.ingest(frameA.frames[0])
        try panelB.ingest(frameB.frames[0])
        #expect(panelA.seq == panelB.seq)
        #expect(panelA.pixels == panelB.pixels)
        #expect(panelA.width == 4 && panelA.height == 2)

        var latch = ButtonLatch()
        let up = latch.down("up")
        let ok = latch.down("ok")
        #expect(up && ok)
        for name in latch.releaseAll() {
            _ = try first.request([("method", .string("button.up")), ("button", .string(name))])
        }
        #expect(server.buttons() == ["ok", "up"])
    }

    /// Boots the NOTE4 demo when `NOTE_G4_LIVE=1`. The default `swift test` skips it.
    @Test func liveNote4ReturnsAFullGray4Frame() throws {
        guard ProcessInfo.processInfo.environment["NOTE_G4_LIVE"] == "1" else { return }
        let repo = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let firmware = repo.appendingPathComponent("third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin")
        let ndb = repo.appendingPathComponent("target/debug/ndb")
        let emu = repo.appendingPathComponent("target/debug/note-emu")
        try #require(FileManager.default.isExecutableFile(atPath: ndb.path))
        try #require(FileManager.default.isExecutableFile(atPath: emu.path))
        try #require(FileManager.default.fileExists(atPath: firmware.path))
        let home = URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
            .appendingPathComponent("note-g4-live-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let rom = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/NOTE Emulator/rom")
        try FileManager.default.createSymbolicLink(at: home.appendingPathComponent("rom"), withDestinationURL: rom)
        let env = ProcessInfo.processInfo.environment.merging(["NOTE_EMU_HOME": home.path]) { _, new in new }
        let created = try run(ndb, ["avd", "create", "--profile", "note4", "--firmware", firmware.path, "--name", "g4"], env)
        let id = created.trimmingCharacters(in: .whitespacesAndNewlines)
        let process = Process()
        process.executableURL = emu
        process.arguments = ["--avd", id, "--seconds", "0"]
        process.environment = env
        let pipe = Pipe()
        process.standardError = pipe
        try process.run()
        defer {
            if process.isRunning { process.terminate() }
        }
        var sock = ""
        for _ in 0..<100 {
            if let record = DataHome.listInstances(home).first(where: { $0.avd == id }) {
                sock = record.sock
                break
            }
            if !process.isRunning {
                let err = String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
                Issue.record("note-emu exited: \(err)")
                return
            }
            Thread.sleep(forTimeInterval: 0.1)
        }
        try #require(!sock.isEmpty)
        let client = ControlClient()
        client.timeout = 10
        defer { client.shutdown() }
        try client.connect(sock)
        let hello = try client.request([("method", .string("hello")), ("protocol", .int(1)), ("client", .string("NoteEmulator"))])
        #expect(jsonValue(jsonObject(hello.json), "engine") == "esp32sim")
        let reply = try client.request([("method", .string("display.get"))])
        var panel = PanelState()
        try panel.ingest(reply.frames[0])
        #expect(panel.width == 400)
        #expect(panel.height == 300)
        #expect(panel.format == FrameFormat.gray4)
        #expect(panel.pixels.count == 60_000)
        _ = try client.request([("method", .string("button.down")), ("button", .string("ok"))])
        _ = try client.request([("method", .string("button.up")), ("button", .string("ok"))])
        _ = try client.request([("method", .string("stop"))])
    }
}

private func run(_ tool: URL, _ arguments: [String], _ environment: [String: String]) throws -> String {
    let process = Process()
    process.executableURL = tool
    process.arguments = arguments
    process.environment = environment
    let out = Pipe()
    process.standardOutput = out
    process.standardError = Pipe()
    try process.run()
    process.waitUntilExit()
    let text = String(data: out.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
    try #require(process.terminationStatus == 0, "\(tool.lastPathComponent) failed: \(text)")
    return text
}

final class MockControl: @unchecked Sendable {
    let path: String
    private var listenFD: Int32 = -1
    private let lock = NSLock()
    private var ups: [String] = []
    private var running = true

    init() throws {
        path = "/tmp/note-g4-\(UUID().uuidString).sock"
        listenFD = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard listenFD >= 0 else { throw ControlError.socket("socket") }
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8)
        withUnsafeMutablePointer(to: &addr.sun_path) { raw in
            raw.withMemoryRebound(to: UInt8.self, capacity: 104) { dst in
                for (index, byte) in bytes.enumerated() { dst[index] = byte }
            }
        }
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                Darwin.bind(listenFD, sa, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard bound == 0, Darwin.listen(listenFD, 4) == 0 else { throw ControlError.socket("bind") }
        let fd = listenFD
        Thread.detachNewThread { [weak self] in
            while let self, self.running {
                let client = Darwin.accept(fd, nil, nil)
                if client < 0 { return }
                Thread.detachNewThread { self.serve(client) }
            }
        }
    }

    func buttons() -> [String] {
        lock.lock()
        defer { lock.unlock() }
        return ups
    }

    func stop() {
        running = false
        if listenFD >= 0 { Darwin.close(listenFD) }
        Darwin.unlink(path)
    }

    private func serve(_ fd: Int32) {
        var decoder = Decoder()
        var buf = [UInt8](repeating: 0, count: 8192)
        while true {
            let count = Darwin.read(fd, &buf, buf.count)
            if count <= 0 { Darwin.close(fd); return }
            decoder.feed(buf.prefix(count))
            while let message = try? decoder.next() {
                guard message.kind == .request else { continue }
                let text = String(decoding: message.payload, as: UTF8.self)
                let payload: [UInt8]
                var extra: [UInt8] = []
                if text.contains("display.get") {
                    payload = Array(#"{"ok":true,"seq":2,"base_seq":0,"full":true,"width":4,"height":2,"epoch":1,"bytes":4}"#.utf8)
                    extra = (try? sampleFrame()) ?? []
                } else if text.contains("button.up") {
                    if let range = text.range(of: "\"button\":\"") {
                        let name = text[range.upperBound...].prefix { $0 != "\"" }
                        lock.lock()
                        ups.append(String(name))
                        lock.unlock()
                    }
                    payload = Array(#"{"ok":true}"#.utf8)
                } else {
                    payload = Array(#"{"ok":true,"engine":"mock","profile":{"id":"note4"}}"#.utf8)
                }
                let response = (try? Message(kind: .response, requestID: message.requestID, payload: payload).encoded()) ?? []
                _ = response.withUnsafeBytes { Darwin.write(fd, $0.baseAddress, response.count) }
                if !extra.isEmpty {
                    let frame = (try? Message(kind: .frame, requestID: message.requestID, payload: extra).encoded()) ?? []
                    _ = frame.withUnsafeBytes { Darwin.write(fd, $0.baseAddress, frame.count) }
                }
            }
        }
    }
}

private func sampleFrame() throws -> [UInt8] {
    let pixels: [UInt8] = [0x10, 0x32, 0x54, 0x76]
    let header = FrameHeader(
        instanceHash: 1, epoch: 1, seq: 2, baseSeq: 0, virtualNs: 0, hostNs: 0,
        width: 4, height: 2, format: FrameFormat.gray4, paletteID: 0, source: 1, refresh: 1,
        dirtyX: 0, dirtyY: 0, dirtyW: 4, dirtyH: 2, pixelHash: DisplayFrame.fnv1a(pixels)
    )
    return try DisplayFrame.encode(header, pixels)
}

@Suite struct SkinTests {
    private func repoProfiles() -> URL {
        URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("../../../Profiles").standardized
    }

    @Test func bothProfilesShareTheShellAndHitTestTheirButtons() throws {
        for id in ["note4", "note4c"] {
            let dir = repoProfiles()
            let skin = try SkinGeometry.decode(profileJSON: Data(contentsOf: dir.appendingPathComponent("\(id).json")), profilesDir: dir)
            #expect(skin.canvas == 1024)
            #expect(skin.screen == CGRect(x: 105, y: 102, width: 815, height: 613))
            #expect(skin.bounds == CGRect(x: 40, y: 40, width: 947, height: 955), "the window hugs the shell")
            #expect(FileManager.default.fileExists(atPath: skin.image.path))
            #expect(skin.button(at: CGPoint(x: 890, y: 891)) == "ok")
            #expect(skin.button(at: CGPoint(x: 975, y: 670)) == "up")
            #expect(skin.button(at: CGPoint(x: 975, y: 820)) == "down")
            #expect(skin.button(at: CGPoint(x: 500, y: 400)) == nil, "the screen is not a button")
            #expect(skin.leds.map(\.name) == ["power"])
        }
    }

    /// The device window's 14 pt resize corner must not cover a key at any window size
    /// (fit minimum 320 pt wide up to 2x).
    @Test func resizeCornerNeverCoversAKey() throws {
        for id in ["note4", "note4c"] {
            let dir = repoProfiles()
            let skin = try SkinGeometry.decode(profileJSON: Data(contentsOf: dir.appendingPathComponent("\(id).json")), profilesDir: dir)
            for width in stride(from: 320.0, through: 2 * skin.bounds.width, by: 20) {
                let scale = width / skin.bounds.width
                let side = 14 / scale
                for step in 0...14 {
                    for other in 0...14 {
                        let point = CGPoint(x: skin.bounds.maxX - side * Double(step) / 14,
                                            y: skin.bounds.maxY - side * Double(other) / 14)
                        #expect(skin.button(at: point) == nil, "\(id) at \(Int(width)) pt: \(point)")
                    }
                }
            }
        }
    }

    @Test func profilesDirectoryHonoursTheEnvironment() {
        #expect(SkinGeometry.profilesDirectory(environment: ["NOTE_EMU_PROFILES": "/x/Profiles"])?.path == "/x/Profiles")
    }
}

@Suite struct BinaryLookupTests {
    @Test func theAppBundlesNoteEmuBesideItsExecutable() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("macos-\(UUID())")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let emu = dir.appendingPathComponent("note-emu")
        FileManager.default.createFile(atPath: emu.path, contents: Data(), attributes: [.posixPermissions: 0o755])
        let found = DataHome.noteEmuBinary(environment: [:], cwd: URL(fileURLWithPath: "/"), executableDir: dir)
        #expect(found?.path == emu.path)
    }
}

@Suite struct RomStateTests {
    @Test func romInstalledLooksForTheImportedElf() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("home-\(UUID())")
        defer { try? FileManager.default.removeItem(at: root) }
        #expect(!DataHome.romInstalled(root))
        try FileManager.default.createDirectory(at: root.appendingPathComponent("rom"), withIntermediateDirectories: true)
        FileManager.default.createFile(atPath: root.appendingPathComponent("rom/esp32s3_rev0_rom.elf").path, contents: Data([0x7f]))
        #expect(DataHome.romInstalled(root))
    }
}

@Suite struct EpochTests {
    private func header(epoch: UInt32, seq: UInt64, base: UInt64, _ pixels: [UInt8]) -> FrameHeader {
        FrameHeader(instanceHash: 1, epoch: epoch, seq: seq, baseSeq: base, virtualNs: 0, hostNs: 0,
                    width: 4, height: 2, format: FrameFormat.gray4, paletteID: 0, source: 1, refresh: 1,
                    dirtyX: 0, dirtyY: 0, dirtyW: 4, dirtyH: 2, pixelHash: DisplayFrame.fnv1a(pixels))
    }

    /// After a snapshot restore the runtime starts a new epoch with a full frame.
    @Test func aFullFrameInANewEpochReplacesTheImageButADeltaDoesNot() throws {
        var panel = PanelState()
        try panel.ingest(try DisplayFrame.encode(header(epoch: 1, seq: 5, base: 0, [1, 1, 1, 1]), [1, 1, 1, 1]))
        let delta = try DisplayFrame.encode(header(epoch: 2, seq: 6, base: 5, [2, 2, 2, 2]), [2, 2, 2, 2])
        #expect(throws: FrameError.self) { try panel.ingest(delta) }
        try panel.ingest(try DisplayFrame.encode(header(epoch: 2, seq: 6, base: 0, [3, 3, 3, 3]), [3, 3, 3, 3]))
        #expect(panel.epoch == 2 && panel.pixels == [3, 3, 3, 3])
    }
}
