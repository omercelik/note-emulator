import AppKit
import NoteMedia
import NoteProtocol
import NoteSession
import Observation
import SwiftUI

struct LogLine: Identifiable, Equatable {
    var seq: UInt64
    var text: String
    /// ESP-IDF level letter (E W I D V) when the line has one.
    var level: String = ""
    var tag: String = ""
    /// 0 UART0, 1 USB (a line seen on both lists both).
    var channels: [Int] = []
    var id: UInt64 { seq }
}

@MainActor
@Observable
final class DeviceModel {
    let socketPath: String
    private let client = ControlClient()
    /// A second connection that only receives pushed frames (`display.subscribe`).
    private let pushClient = ControlClient()
    private var push: Task<Void, Never>?
    /// When the last pushed frame arrived; while pushes flow the poll loop skips `display.get`.
    private var lastPush = Date.distantPast
    private(set) var pushedFrames = 0
    private var latch = ButtonLatch()
    private var poll: Task<Void, Never>?

    var panel = PanelState()
    var image: CGImage?
    var lines: [LogLine] = []
    var heldButtons: Set<String> = []
    var connected = false {
        didSet { if connected != oldValue { updateMicrophone() } }
    }
    var engine = ""
    var profile = ""
    var paused = false
    var virtualSeconds = 0.0
    var batteryMV = 3900
    var reportedMV = 3900
    var networkConfigured = "disabled"
    var networkActive = "disabled"
    var browserURL = ""
    var restartRequired = false
    var lastError = ""
    var query = ""
    /// Minimum ESP-IDF level to show: "" all, else one of E W I D V.
    var minLevel = ""
    /// -1 both channels, 0 UART0, 1 USB.
    var channel = -1
    var skin: SkinGeometry?
    /// The profile `skin` was loaded for.
    private var skinProfile = ""
    /// The AVD's name, when the manager opened the window.
    var displayName = ""
    /// Device window view: 0 fit, 1 = 1×, 2 = 2× panel pixels; rotation in quarter turns.
    var zoom = 0
    var rotation = 0

    var windowTitle: String {
        if !displayName.isEmpty { return displayName }
        return profile.isEmpty ? "Device" : profile.uppercased()
    }

    /// For file names of screenshots, recordings and log exports.
    var fileStem: String {
        let base = displayName.isEmpty ? (profile.isEmpty ? "note" : profile) : displayName
        return base.replacingOccurrences(of: "/", with: "-")
    }
    var leds: [String: Bool] = [:]
    var recording = false
    var snapshots: [String] = []
    var usbCable = false
    var usbHost = true
    var speakerMuted = false {
        didSet { speaker.muted = speakerMuted }
    }
    var speakerActive = false
    /// Speaker samples received from the device and queued for playback.
    var speakerSamples: UInt64 = 0
    private let speaker = SpeakerPlayer()
    /// The Mac's microphone feeds the device's while on (Audio controls).
    /// Mac microphone capture is running (it feeds the device's microphone).
    var macMicrophone = false {
        didSet { if macMicrophone != oldValue { macMicrophone ? startMicrophone() : stopMicrophone() } }
    }
    /// "Use this Mac's microphone", per device and remembered (default on). Capture runs only
    /// while the firmware is listening, so macOS asks for permission on first real use.
    var micEnabled = true {
        didSet {
            if let avdID { UserDefaults.standard.set(micEnabled, forKey: "microphone.\(avdID)") }
            updateMicrophone()
        }
    }
    /// The firmware is reading its microphone (reported by `status`).
    var micListening = false {
        didSet { if micListening != oldValue { updateMicrophone() } }
    }
    /// The AVD, from the registry (for per-device preferences).
    private(set) var avdID: String?
    var micLevel: Float = 0
    private var mic: (any MicrophoneCapture)?
    private let microphoneAccess: @MainActor () async -> Bool
    private let makeMicrophone: @MainActor () -> any MicrophoneCapture
    private let connectMicrophone: @MainActor (ControlClient, String) async throws -> Void
    private var micGeneration: UInt64 = 0
    private(set) var micTask: Task<Void, Never>?
    /// The microphone streams on its own connection so it never interleaves with the poll loop.
    private var micClient: ControlClient?
    private var audioNext: UInt64?
    var snapshotStatus = ""
    private var recorder: FrameRecorder?

    init(socketPath: String,
         microphoneAccess: @escaping @MainActor () async -> Bool = { await MacMicrophone.requestAccess() },
         makeMicrophone: @escaping @MainActor () -> any MicrophoneCapture = { MacMicrophone() },
         connectMicrophone: @escaping @MainActor (ControlClient, String) async throws -> Void = { client, path in
             try await Task.detached { try client.connect(path) }.value
         }) {
        self.socketPath = socketPath
        self.microphoneAccess = microphoneAccess
        self.makeMicrophone = makeMicrophone
        self.connectMicrophone = connectMicrophone
        // Show the right shell while connecting: the registry names the AVD, whose config names
        // the profile. `hello` confirms it once the device answers.
        let root = DataHome.url()
        avdID = DataHome.listInstances(root).first(where: { $0.sock == socketPath })?.avd
        if let avdID, let saved = UserDefaults.standard.object(forKey: "microphone.\(avdID)") as? Bool {
            micEnabled = saved
        }
        let record = avdID.flatMap { avd in DataHome.listAvds(root).first { $0.id == avd } }
        // Windows opened from a socket alone (Controls, a relaunch) still show the AVD's name.
        if let name = record?.name { displayName = name }
        if let avd = avdID,
           let known = DataHome.listAvds(root).first(where: { $0.id == avd })?.profile {
            profile = known
            skin = SkinGeometry.load(profileID: known)
            skinProfile = known
        }
    }

    var visibleLines: [LogLine] {
        let order = ["E": 0, "W": 1, "I": 2, "D": 3, "V": 4]
        return lines.filter { line in
            if !query.isEmpty && !line.text.localizedCaseInsensitiveContains(query) && !line.tag.localizedCaseInsensitiveContains(query) { return false }
            if channel >= 0 && !line.channels.isEmpty && !line.channels.contains(channel) { return false }
            if let limit = order[minLevel] {
                guard let lv = order[line.level] else { return false }
                if lv > limit { return false }
            }
            return true
        }
    }

    /// The lines as shown (after filters), one per row, for Export….
    func exportText() -> String {
        visibleLines.map(\.text).joined(separator: "\n") + "\n"
    }

    /// Stop polling and close the connections; the emulator keeps running (window closed).
    func detach() {
        macMicrophone = false
        poll?.cancel()
        push?.cancel()
        client.shutdown()
        pushClient.shutdown()
        speaker.stop()
        connected = false
    }

    private func updateMicrophone() {
        let want = micEnabled && micListening && connected
        if want != macMicrophone { macMicrophone = want }
    }

    private func startMicrophone() {
        micGeneration &+= 1
        let generation = micGeneration
        // Each attempt owns its connection; an old cancelled task cannot close a new one.
        let client = ControlClient()
        micClient = client
        micTask = Task {
            var capture: (any MicrophoneCapture)?
            defer {
                capture?.stop()
                client.shutdown()
                if micGeneration == generation {
                    mic = nil
                    micClient = nil
                    micLevel = 0
                }
            }
            let allowed = await microphoneAccess()
            guard !Task.isCancelled, micGeneration == generation else { return }
            guard allowed else {
                lastError = "Microphone access is off for NOTE Emulator (System Settings ▸ Privacy & Security ▸ Microphone)."
                // Declined: stop asking until the user turns the microphone on again.
                micEnabled = false
                macMicrophone = false
                return
            }
            do {
                // Connect before activating the audio hardware.
                try await connectMicrophone(client, socketPath)
                try Task.checkCancellation()
                guard micGeneration == generation else { return }
                let microphone = makeMicrophone()
                capture = microphone
                try microphone.start()
                mic = microphone
                while !Task.isCancelled {
                    try await Task.sleep(for: .milliseconds(60))
                    let chunk = microphone.take()
                    micLevel = microphone.level
                    guard !chunk.isEmpty else { continue }
                    let pairs: [(String, JSONField)] = [
                        ("method", .string("audio.mic")), ("pcm", .string(chunk.base64EncodedString())),
                        ("rate", .int(MacMicrophone.rate)),
                    ]
                    _ = try await Task.detached { try client.request(pairs) }.value
                }
            } catch {
                guard !Task.isCancelled, micGeneration == generation else { return }
                lastError = "microphone: \(error)"
                macMicrophone = false
            }
        }
    }

    private func stopMicrophone() {
        micGeneration &+= 1
        micTask?.cancel()
        micTask = nil
        mic?.stop()
        mic = nil
        micClient?.shutdown()
        micClient = nil
        micLevel = 0
    }

    func deleteSnapshot(_ name: String) async {
        do {
            _ = try await call([("method", .string("snapshot.delete")), ("name", .string(name))])
            snapshotStatus = "Deleted \(name)"
        } catch {
            snapshotStatus = String(describing: error)
        }
        await refreshSnapshots()
    }

    func start() {
        poll?.cancel()
        poll = Task { await self.run() }
    }

    func press(_ button: String, down: Bool) async {
        let send = down ? latch.down(button) : latch.up(button)
        heldButtons = latch.held
        guard send else { return }
        let method = down ? "button.down" : "button.up"
        _ = try? await call([("method", .string(method)), ("button", .string(button))])
        await pullFrame()
    }

    /// Key window resigned, or this window closed. Every held button goes up.
    func releaseFocus() async {
        let names = latch.releaseAll()
        heldButtons = []
        guard !names.isEmpty else { return }
        for name in names {
            _ = try? await call([("method", .string("button.up")), ("button", .string(name))])
        }
        await pullFrame()
    }

    func togglePause() async {
        let method = paused ? "resume" : "pause"
        _ = try? await call([("method", .string(method))])
    }

    /// Reset the chip (the reset pinhole): the firmware reboots; flash and settings stay.
    func restart() async {
        _ = try? await call([("method", .string("boot.reset")), ("mode", .string("normal"))])
    }

    func stop() async {
        _ = try? await call([("method", .string("stop"))])
        connected = false
        poll?.cancel()
        push?.cancel()
        pushClient.shutdown()
        speaker.stop()
        client.shutdown()
    }

    /// Plug or unplug the USB cable (supply + charger) and/or the USB host (console).
    func setUSB(cable: Bool? = nil, host: Bool? = nil) async {
        var pairs: [(String, JSONField)] = [("method", .string("power.usb"))]
        if let cable { pairs.append(("cable", .bool(cable))) }
        if let host { pairs.append(("host", .bool(host))) }
        if let reply = try? await call(pairs) {
            let object = jsonObject(reply.json)
            usbCable = jsonBool(object, "cable")
            usbHost = jsonBool(object, "host")
        }
    }

    func applyBattery() async {
        let mv = batteryMV
        _ = try? await call([("method", .string("battery.set")), ("mv", .int(mv))])
    }

    func configureNetwork(_ mode: String) async {
        _ = try? await call([("method", .string("network.configure")), ("mode", .string(mode))])
    }

    func clearView() async {
        lines = []
        _ = try? await call([("method", .string("logs.clear_view"))])
    }

    func sendConsole(_ text: String, ending: String) async {
        let line = text + ending
        _ = try? await call([("method", .string("console.write")), ("channel", .string("uart0")), ("text", .string(line))])
    }

    /// Full machine snapshot into the AVD (`snapshot.save`).
    func saveSnapshot(_ name: String) async {
        do {
            let reply = try await call([("method", .string("snapshot.save")), ("name", .string(name))])
            let object = jsonObject(reply.json)
            if let error = object["error"] as? [String: Any] {
                snapshotStatus = jsonValue(error, "message")
            } else {
                snapshotStatus = "Saved \(name) at \(String(format: "%.1f", virtualSeconds)) s"
            }
        } catch {
            snapshotStatus = String(describing: error)
        }
        await refreshSnapshots()
    }

    /// Restore a snapshot; the runtime starts a new frame epoch, so the next poll redraws.
    func loadSnapshot(_ name: String) async {
        do {
            let reply = try await call([("method", .string("snapshot.load")), ("name", .string(name))])
            let object = jsonObject(reply.json)
            if let error = object["error"] as? [String: Any] {
                snapshotStatus = jsonValue(error, "message")
            } else {
                snapshotStatus = "Loaded \(name)"
                await pullFrame()
            }
        } catch {
            snapshotStatus = String(describing: error)
        }
    }

    func refreshSnapshots() async {
        guard let reply = try? await call([("method", .string("snapshot.list"))]) else { return }
        let rows = jsonObject(reply.json)["snapshots"] as? [[String: Any]] ?? []
        snapshots = rows.map { jsonValue($0, "name") }.filter { $0 != "quickboot" }
    }

    /// PNG of the canonical frame (panel pixels, no skin).
    func saveScreenshot(to url: URL) throws {
        guard let image else { throw RecorderError.failed("no frame yet") }
        guard let dest = CGImageDestinationCreateWithURL(url as CFURL, "public.png" as CFString, 1, nil) else {
            throw RecorderError.failed("create \(url.path)")
        }
        CGImageDestinationAddImage(dest, image, nil)
        guard CGImageDestinationFinalize(dest) else { throw RecorderError.failed("write \(url.path)") }
    }

    /// MP4 of committed panel states on virtual time (CAP-01); a pause adds no length.
    func startRecording(to url: URL) {
        do {
            let recorder = try FrameRecorder(url: url, width: max(panel.width, 2), height: max(panel.height, 2))
            if let image { try recorder.append(image, virtualSeconds: virtualSeconds) }
            self.recorder = recorder
            recording = true
        } catch {
            lastError = "record: \(error)"
        }
    }

    func stopRecording() async {
        guard let recorder else { return }
        self.recorder = nil
        recording = false
        do {
            try await recorder.finish(virtualSeconds: virtualSeconds)
        } catch {
            lastError = "record: \(error)"
        }
    }

    private func run() async {
        do {
            try await connectWhenReady()
            let hello = try await call([("method", .string("hello")), ("protocol", .int(1)), ("client", .string("NoteEmulator"))])
            let object = jsonObject(hello.json)
            engine = jsonValue(object, "engine")
            if let profileObject = object["profile"] as? [String: Any] {
                profile = jsonValue(profileObject, "id")
            }
            if skin == nil || skinProfile != profile {
                skin = SkinGeometry.load(profileID: profile)
                skinProfile = profile
            }
            connected = true
            startPush()
            await pullFrame()
            await pullLogs()
            await refreshSnapshots()
            var tick = 0
            while !Task.isCancelled && connected {
                await pullStatus()
                if Date().timeIntervalSince(lastPush) > 1 { await pullFrame() }
                await pullAudio()
                tick += 1
                if tick % 4 == 0 { await pullLogs() }
                try? await Task.sleep(for: .milliseconds(250))
            }
        } catch {
            lastError = String(describing: error)
            connected = false
        }
    }

    private func connectWhenReady() async throws {
        var last = ""
        for _ in 0..<50 {
            do {
                try client.connect(socketPath)
                return
            } catch {
                last = String(describing: error)
                try? await Task.sleep(for: .milliseconds(100))
            }
        }
        throw ControlError.socket(last)
    }

    /// Subscribe on a second connection and apply every frame the runtime pushes. If that fails
    /// the window keeps polling.
    private func startPush() {
        push?.cancel()
        let client = pushClient
        let path = socketPath
        push = Task.detached { [weak self] in
            do {
                try client.connect(path)
                _ = try client.request([("method", .string("display.subscribe"))])
            } catch {
                return
            }
            while !Task.isCancelled {
                guard let frames = try? client.pushedFrames() else { return }
                if frames.isEmpty { continue }
                await self?.applyPushed(frames)
            }
        }
    }

    private func applyPushed(_ frames: [[UInt8]]) {
        for frame in frames {
            do {
                let heldSeq = panel.seq
                try panel.ingest(frame)
                image = cgImage(rgba: panel.rgba(), width: panel.width, height: panel.height)
                pushedFrames += 1
                lastPush = Date()
                if let recorder, let image, panel.seq != heldSeq {
                    try recorder.append(image, virtualSeconds: Double(panel.virtualNs) / 1e9)
                }
            } catch {
                lastError = String(describing: error)
            }
        }
    }

    private func pullFrame() async {
        guard let reply = try? await call([("method", .string("display.get"))]) else { return }
        for frame in reply.frames {
            do {
                let heldSeq = panel.seq
                try panel.ingest(frame)
                image = cgImage(rgba: panel.rgba(), width: panel.width, height: panel.height)
                if let recorder, let image, panel.seq != heldSeq || recorder.frames == 0 {
                    try recorder.append(image, virtualSeconds: Double(panel.virtualNs) / 1e9)
                }
            } catch {
                lastError = String(describing: error)
            }
        }
    }

    /// Speaker samples since the last poll, played on the Mac. The first poll starts at "now".
    private func pullAudio() async {
        var pairs: [(String, JSONField)] = [("method", .string("audio.get"))]
        if let next = audioNext { pairs.append(("from", .int(Int(next)))) }
        guard let reply = try? await call(pairs) else { return }
        let object = jsonObject(reply.json)
        if let next = jsonInt(object, "next") { audioNext = UInt64(next) }
        let rate = Double(jsonInt(object, "rate") ?? 0)
        let samples = reply.audio.flatMap(SpeakerPlayer.samples)
        speakerActive = !samples.isEmpty
        if !samples.isEmpty {
            speakerSamples += UInt64(samples.count)
            speaker.play(samples, rate: rate)
        }
    }

    private func pullLogs() async {
        guard let reply = try? await call([("method", .string("logs.export")), ("view", .bool(true))]) else { return }
        let rows = jsonObject(reply.json)["lines"] as? [[String: Any]] ?? []
        var known = Set(lines.map(\.seq))
        for row in rows {
            let seqs = row["seqs"] as? [Any] ?? []
            let seq = seqs.compactMap { ($0 as? NSNumber)?.uint64Value }.last ?? UInt64(lines.count)
            let text = row["text"] as? String ?? ""
            if known.insert(seq).inserted {
                let channels = (row["channels"] as? [Any] ?? []).compactMap { ($0 as? NSNumber)?.intValue }
                lines.append(LogLine(seq: seq, text: text, level: row["level"] as? String ?? "", tag: row["tag"] as? String ?? "", channels: channels))
            }
        }
        if lines.count > 2000 { lines.removeFirst(lines.count - 2000) }
    }

    private func pullStatus() async {
        if let reply = try? await call([("method", .string("status"))]) {
            let object = jsonObject(reply.json)
            paused = jsonBool(object, "paused")
            if let ns = jsonInt(object, "virtual_ns") { virtualSeconds = Double(ns) / 1_000_000_000 }
            if let mv = jsonInt(object, "battery_mv") { reportedMV = mv }
            if object["usb_cable"] != nil { usbCable = jsonBool(object, "usb_cable") }
            if object["usb_host"] != nil { usbHost = jsonBool(object, "usb_host") }
            micListening = jsonBool(object, "mic_listening")
            var lit: [String: Bool] = [:]
            for led in object["leds"] as? [[String: Any]] ?? [] {
                lit[jsonValue(led, "led")] = jsonBool(led, "lit")
            }
            leds = lit
        }
        if let reply = try? await call([("method", .string("network.info"))]) {
            let object = jsonObject(reply.json)
            networkConfigured = jsonValue(object, "configured")
            networkActive = jsonValue(object, "active")
            browserURL = jsonValue(object, "browser_url")
            restartRequired = jsonBool(object, "restart_required")
        }
        if let reply = try? await call([("method", .string("battery.get"))]) {
            if let mv = jsonInt(jsonObject(reply.json), "mv") { reportedMV = mv }
        }
    }

    private func call(_ pairs: [(String, JSONField)]) async throws -> ControlReply {
        let client = self.client
        return try await Task.detached {
            try client.request(pairs)
        }.value
    }
}

func cgImage(rgba: [UInt8], width: Int, height: Int) -> CGImage? {
    guard width > 0, height > 0, rgba.count == width * height * 4 else { return nil }
    let data = Data(rgba) as CFData
    guard let provider = CGDataProvider(data: data) else { return nil }
    return CGImage(
        width: width,
        height: height,
        bitsPerComponent: 8,
        bitsPerPixel: 32,
        bytesPerRow: width * 4,
        space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
        provider: provider,
        decode: nil,
        shouldInterpolate: false,
        intent: .defaultIntent
    )
}
