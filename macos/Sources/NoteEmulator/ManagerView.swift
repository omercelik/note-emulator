import AppKit
import NoteSession
import Observation
import SwiftUI

struct DeviceLink: Codable, Hashable {
    var sock: String
    /// The AVD's name, for the window title. Not part of the identity.
    var name: String = ""

    // One window per running device: opening it again brings that window forward.
    static func == (a: DeviceLink, b: DeviceLink) -> Bool { a.sock == b.sock }
    func hash(into hasher: inout Hasher) { hasher.combine(sock) }
}

/// Last chunk of a child's stderr. The readability handler is not on the main actor.
final class ManagerTail: @unchecked Sendable {
    private let lock = NSLock()
    private var bytes = Data()

    func append(_ data: Data) {
        lock.lock()
        bytes.append(data)
        if bytes.count > 4096 { bytes.removeFirst(bytes.count - 4096) }
        lock.unlock()
    }

    var text: String {
        lock.lock()
        let copy = bytes
        lock.unlock()
        let rendered = String(data: copy, encoding: .utf8) ?? ""
        let trimmed = rendered.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "note-emu exited" : trimmed
    }
}

@MainActor
@Observable
final class ManagerModel {
    /// One per app: the manager window and every Controls window start devices through it.
    static let shared = ManagerModel()
    var avds: [AvdRecord] = []
    var instances: [InstanceRecord] = []
    var message = ""
    var launching = ""
    /// AVDs asked to stop whose emulator has not exited yet.
    var stopping: Set<String> = []
    var deleting: Set<String> = []
    var romInstalled = true
    private var children: [String: Process] = [:]
    @ObservationIgnored private var quitObserver: NSObjectProtocol?

    /// Quitting stops the emulators this app started, whichever windows are open. (Hanging this
    /// on the manager window's view missed quits with that window closed and orphaned them.)
    init() {
        quitObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.stopLaunched() }
        }
    }

    func reload() {
        let root = DataHome.url()
        avds = DataHome.listAvds(root)
        NetworkLaunchSettings.shared.invalidate()
        instances = DataHome.listInstances(root)
        romInstalled = DataHome.romInstalled(root)
    }

    /// `ndb rom import` (it checks the ELF's SHA-256 against the known esp-rom-elfs release).
    /// The ROM ships with the app (ADR-018): import it quietly the first time; the banner only
    /// shows if there is no bundled copy or it fails its check.
    func importBundledRomIfNeeded() async {
        reload()
        guard !romInstalled else { return }
        _ = await ndb(["rom", "import"])
        if romInstalled || DataHome.romInstalled(DataHome.url()) { message = "" }
        reload()
    }

    /// The NOTE4 reference demo shipped with the app (MIT, Zectrix Lab), or the repository's copy
    /// when run from a build tree.
    var sampleFirmware: URL? {
        sampleFirmware(named: "zectrix-note4-epd-demo-v1.0.0.bin", repositoryPath: "third_party/zectrix-note4-epd-demo")
    }

    var note4cSampleFirmware: URL? {
        sampleFirmware(named: "emini-home-0.6.2-note4c-merged.bin", repositoryPath: "third_party/emini-home")
    }

    private func sampleFirmware(named name: String, repositoryPath: String) -> URL? {
        if let bundled = Bundle.main.resourceURL?.appendingPathComponent("samples/\(name)"),
           FileManager.default.fileExists(atPath: bundled.path) { return bundled }
        var dir = Bundle.main.executableURL?.deletingLastPathComponent()
        for _ in 0..<6 {
            guard let current = dir else { break }
            let candidate = current.appendingPathComponent("\(repositoryPath)/\(name)")
            if FileManager.default.fileExists(atPath: candidate.path) { return candidate }
            dir = current.deletingLastPathComponent()
        }
        return nil
    }

    func importRom(_ url: URL) async {
        if await ndb(["rom", "import", url.path]) != nil { message = "" }
        reload()
    }

    /// `ndb avd create`. Returns the new AVD id.
    func createAvd(profile: String, firmware: URL, name: String, network: String? = nil) async -> String? {
        var args = ["avd", "create", "--profile", profile, "--firmware", firmware.path]
        if !name.isEmpty { args += ["--name", name] }
        let id = await ndb(args)?.trimmingCharacters(in: .whitespacesAndNewlines)
        if let id, let network { _ = await ndb(["avd", "network", id, network]) }
        reload()
        return id
    }

    /// Runs `ndb`; on failure its stderr becomes `message`.
    private func ndb(_ arguments: [String]) async -> String? {
        switch await Ndb.run(arguments) {
        case .success(let out): return out
        case .failure(let error):
            message = error.message
            return nil
        }
    }

    /// Ask a running device to stop (it commits flash and, with quick boot, saves its snapshot).
    func stop(avd: String) async {
        stopping.insert(avd)
        defer { stopping.remove(avd) }
        _ = await ndb(["-s", avd, "stop"])
        // The emulator commits flash (and saves its quick-boot snapshot) before it exits.
        for _ in 0..<150 {
            reload()
            if instance(for: avd) == nil { return }
            try? await Task.sleep(for: .milliseconds(200))
        }
    }

    /// Factory reset: flash back to the imported image, quick-boot state dropped.
    func erase(avd: String) async {
        _ = await ndb(["avd", "wipe", avd])
        reload()
    }

    /// Permanently remove a stopped device and all of its saved data.
    func delete(avd: String) async {
        guard deleting.insert(avd).inserted else { return }
        defer { deleting.remove(avd) }
        if await ndb(["avd", "delete", avd]) != nil { message = "" }
        reload()
    }

    /// Point the AVD's virtual AP at a dotenv file's Wi-Fi credentials, or clear it (nil).
    func setWifiEnv(_ url: URL?, avd: String) async {
        if let url {
            _ = await ndb(["avd", "wifi", avd, "--env", url.path])
        } else {
            _ = await ndb(["avd", "wifi", avd, "--clear"])
        }
        reload()
    }

    /// Save form entries privately inside the device directory; no user-managed file is needed.
    func saveWifiCredentials(ssid: String, password: String, avd: String) async -> Bool {
        guard avds.contains(where: { $0.id == avd }), UUID(uuidString: avd) != nil else {
            message = "Device not found"; return false
        }
        guard !ssid.isEmpty, ssid.utf8.count <= 32,
              !ssid.contains(","), !ssid.unicodeScalars.contains(where: { $0.value < 32 || $0.value == 127 }),
              !password.contains(","), password.utf8.allSatisfy({ $0 >= 32 && $0 <= 126 }),
              password.isEmpty || (8...63).contains(password.utf8.count) else {
            message = "Wi-Fi name must be 1–32 bytes; password must be empty or 8–63 ASCII characters. Commas and control characters are unsupported."
            return false
        }
        do {
            let dir = DataHome.url().appendingPathComponent("avd/\(avd).avd/wifi", isDirectory: true)
            if !FileManager.default.fileExists(atPath: dir.path) {
                try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: false,
                    attributes: [.posixPermissions: 0o700])
            }
            try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: dir.path)
            let file = dir.appendingPathComponent("network.env")
            let text = "WIFI_SSID=\"\(ssid)\"\nWIFI_PASSWORD=\"\(password)\"\n"
            try Data(text.utf8).write(to: file, options: .atomic)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
            guard await ndb(["avd", "wifi", avd, "--env", file.path]) != nil else { return false }
            message = ""
            reload()
            return true
        } catch {
            message = "Could not save Wi-Fi settings: \(error.localizedDescription)"
            return false
        }
    }

    /// The app started this device, so closing its window turns it off. A device started by
    /// `ndb` or `note-emu` only has its window detached.
    func owns(_ avd: String) -> Bool {
        children[avd]?.isRunning == true
    }

    /// Row status, in words.
    func status(of avd: String) -> String {
        if deleting.contains(avd) { return "Deleting…" }
        if launching == avd { return "Starting…" }
        if stopping.contains(avd) { return "Stopping…" }
        guard instance(for: avd) != nil else { return "Stopped" }
        return owns(avd) ? "Running" : "Running outside the app"
    }

    func instance(for avd: String) -> InstanceRecord? {
        instances.first { $0.avd == avd }
    }

    /// Launch `note-emu --avd --seconds 0 --realtime` and return its control socket. Quick boot
    /// resumes from the snapshot saved at the last stop (and saves one at this stop); a cold boot
    /// starts the firmware from reset.
    func start(avd: String, quickBoot: Bool = true) async -> String? {
        if let existing = instance(for: avd) { return existing.sock }
        guard let binary = DataHome.noteEmuBinary(cwd: URL(fileURLWithPath: FileManager.default.currentDirectoryPath)) else {
            message = "note-emu was not found. Build it, or set NOTE_EMU_BIN."
            return nil
        }
        launching = avd
        defer { launching = "" }
        message = ""
        // A device that wants the helper's addresses starts even without the helper: note-emu
        // falls back to a local address. The administrator prompt is only ever asked for from
        // Controls ▸ Network, where the user can see what it is for.
        let result = await launch(binary: binary, avd: avd, quickBoot: quickBoot)
        return result
    }

    private func launch(binary: URL, avd: String, quickBoot: Bool) async -> String? {
        let process = Process()
        process.executableURL = binary
        // The network mode comes from the AVD's config; note-emu applies it per profile.
        // State is always saved at stop; a cold boot only skips resuming it this once.
        process.arguments = ["--avd", avd, "--seconds", "0", "--realtime", "--entropy", "host", "--quick-boot"]
            + (quickBoot ? [] : ["--cold-boot"])
        process.environment = ProcessInfo.processInfo.environment
        // An unread pipe fills and note-emu blocks in its console log. Keep a short tail.
        let pipe = Pipe()
        process.standardError = pipe
        process.standardOutput = FileHandle.nullDevice
        let tail = ManagerTail()
        pipe.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            guard !data.isEmpty else { return }
            tail.append(data)
        }
        do {
            try process.run()
        } catch {
            message = error.localizedDescription
            return nil
        }
        children[avd] = process
        for _ in 0..<60 {
            reload()
            if let record = instance(for: avd) { return record.sock }
            if !process.isRunning {
                pipe.fileHandleForReading.readabilityHandler = nil
                message = tail.text
                children[avd] = nil
                return nil
            }
            try? await Task.sleep(for: .milliseconds(500))
        }
        message = "note-emu did not publish a control socket"
        return nil
    }

    /// Quit stops the emulators this window started. Each is asked to stop (it commits flash
    /// and, with quick boot, saves its snapshot) and given time to exit before it is terminated.
    func stopLaunched() {
        for (avd, _) in children {
            if let record = instance(for: avd) {
                let client = ControlClient()
                client.timeout = 1
                if (try? client.connect(record.sock)) != nil {
                    _ = try? client.request([("method", .string("stop"))])
                }
                client.shutdown()
            }
        }
        let deadline = Date().addingTimeInterval(15)
        while children.values.contains(where: \.isRunning) && Date() < deadline {
            Thread.sleep(forTimeInterval: 0.05)
        }
        for process in children.values where process.isRunning {
            process.terminate() // SIGTERM is still a graceful stop in note-emu
        }
        children.removeAll()
    }
}

struct ManagerView: View {
    @Bindable var model: ManagerModel
    @Environment(\.openWindow) private var openWindow
    @State private var creating = false
    @State private var erasing: AvdRecord?
    @State private var pendingDeletion: AvdRecord?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if !model.romInstalled {
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("ESP32-S3 ROM needed").font(.headline)
                        Text("The emulator runs the chip's own mask ROM, which is not shipped with the app. Choose esp32s3_rev0_rom.elf from ESP-IDF's esp-rom-elfs (release 20241011); it is checked and copied once.")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("Import ROM…") { chooseRom() }
                }
                .padding()
                .background(Color.yellow.opacity(0.12))
            }
            if !model.message.isEmpty {
                Text(model.message)
                    .font(.callout)
                    .foregroundStyle(.red)
                    .padding(.horizontal)
                    .padding(.top, 8)
            }
            List {
                if model.avds.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("No devices yet. Create one with New AVD…, or add a bundled demo below.")
                            .foregroundStyle(.secondary)
                        if let sample = model.sampleFirmware {
                            Button("Add the NOTE4 demo") {
                                Task { _ = await model.createAvd(profile: "note4", firmware: sample, name: "NOTE4 demo") }
                            }
                            .disabled(!model.romInstalled)
                        }
                        if let sample = model.note4cSampleFirmware {
                            Button("Add the NOTE4C emini Home") {
                                Task { _ = await model.createAvd(profile: "note4c", firmware: sample, name: "NOTE4C emini Home", network: "setup") }
                            }
                            .disabled(!model.romInstalled)
                        }
                    }
                    .padding(.vertical, 6)
                }
                ForEach(model.avds) { avd in
                    let running = model.instance(for: avd.id)
                    let busy = model.launching == avd.id || model.stopping.contains(avd.id) || model.deleting.contains(avd.id)
                    HStack(spacing: 10) {
                        Circle()
                            .fill(busy ? Color.orange : (running != nil ? Color.green : Color.secondary.opacity(0.4)))
                            .frame(width: 8, height: 8)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(avd.name).font(.headline)
                            Text("\(avd.profile)  \(avd.id.prefix(8))  ·  \(model.status(of: avd.id))")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        // One action: start it if needed and show its window. Closing the window
                        // turns the device off; its state is saved for the next Open.
                        Button("Open") {
                            if let running { open(running.sock, name: avd.name) } else { launch(avd, quickBoot: true) }
                        }
                        .disabled(busy)
                        .help(running == nil
                              ? "Turn the device on where it left off, and show it"
                              : "Show the device")
                        Menu {
                            Button("Cold Boot") { launch(avd, quickBoot: false) }
                                .disabled(running != nil || busy)
                            Button("Controls…") { openWindow(id: "controls", value: running?.sock ?? "avd:\(avd.id)") }
                            Divider()
                            Button("Turn Off") { Task { await model.stop(avd: avd.id) } }
                                .disabled(running == nil || busy)
                            Button("Erase Content and Settings…") { erasing = avd }
                                .disabled(running != nil || busy)
                            Button("Delete Device…", role: .destructive) { pendingDeletion = avd }
                                .disabled(running != nil || busy)
                            Button("Show in Finder") {
                                let dir = DataHome.url().appendingPathComponent("avd/\(avd.id).avd")
                                NSWorkspace.shared.activateFileViewerSelecting([dir])
                            }
                        } label: {
                            Image(systemName: "ellipsis.circle")
                        }
                        .menuStyle(.borderlessButton)
                        .menuIndicator(.hidden)
                        .fixedSize()
                        .help("More: cold boot, controls, turn off, erase, delete")
                    }
                    .padding(.vertical, 4)
                }
            }
        }
        .navigationTitle("NOTE Emulator")
        .toolbar {
            Menu("Add Bundled Device") {
                if let sample = model.sampleFirmware {
                    Button("NOTE4 reference demo") {
                        Task { _ = await model.createAvd(profile: "note4", firmware: sample, name: "NOTE4 demo") }
                    }
                }
                if let sample = model.note4cSampleFirmware {
                    Button("NOTE4C emini Home") {
                        Task { _ = await model.createAvd(profile: "note4c", firmware: sample, name: "NOTE4C emini Home", network: "setup") }
                    }
                }
            }
            .disabled(!model.romInstalled)
            Button("New AVD…") { creating = true }
        }
        .confirmationDialog("Erase \(erasing?.name ?? "")?", isPresented: Binding(get: { erasing != nil }, set: { if !$0 { erasing = nil } }),
                            titleVisibility: .visible, presenting: erasing) { avd in
            Button("Erase", role: .destructive) { Task { await model.erase(avd: avd.id) } }
        } message: { _ in
            Text("Its flash goes back to the imported firmware image: Wi-Fi setup, pairing and everything the firmware saved are removed, and the next Open is a first boot. Snapshots you saved by name stay.")
        }
        .alert("Delete \(pendingDeletion?.name ?? "device")?",
               isPresented: Binding(get: { pendingDeletion != nil }, set: { if !$0 { pendingDeletion = nil } }),
               presenting: pendingDeletion) { avd in
            Button("Delete Device", role: .destructive) { Task { await model.delete(avd: avd.id) } }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("This permanently removes the device, its firmware copy, settings, flash contents, and all snapshots. The original firmware file stays. This cannot be undone.")
        }
        .sheet(isPresented: $creating) {
            NewAvdSheet(model: model) { creating = false }
        }
        .task {
            openTestDevice()
            await model.importBundledRomIfNeeded()
            while !Task.isCancelled {
                model.reload()
                try? await Task.sleep(for: .seconds(1))
            }
        }
    }

    private func chooseRom() {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = []
        panel.allowsOtherFileTypes = true
        panel.message = "Choose esp32s3_rev0_rom.elf"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task { await model.importRom(url) }
    }

    @MainActor private static var openedTestDevice = false

    /// `NOTE_TEST_DEVICE_SOCK` opens that instance's device window at launch (visual checks of
    /// the real device window, e.g. transparency).
    private func openTestDevice() {
        guard !Self.openedTestDevice else { return }
        Self.openedTestDevice = true
        guard let sock = ProcessInfo.processInfo.environment["NOTE_TEST_DEVICE_SOCK"], !sock.isEmpty else { return }
        open(sock, name: "Test device")
    }

    private func launch(_ avd: AvdRecord, quickBoot: Bool) {
        Task { if let sock = await model.start(avd: avd.id, quickBoot: quickBoot) { open(sock, name: avd.name) } }
    }

    private func open(_ sock: String, name: String) {
        openWindow(id: "device", value: DeviceLink(sock: sock, name: name))
    }
}

/// Profile + firmware (image or ESP-IDF build directory) → `ndb avd create`.
struct NewAvdSheet: View {
    let model: ManagerModel
    let done: () -> Void
    @State private var profile = "note4"
    @State private var firmware: URL?
    @State private var name = ""
    @State private var busy = false

    var body: some View {
        Form {
            Picker("Device", selection: $profile) {
                Text("NOTE4 (black/white, 16 gray)").tag("note4")
                Text("NOTE4C (four colour)").tag("note4c")
            }
            HStack {
                Text(firmware?.lastPathComponent ?? "No firmware chosen").foregroundStyle(firmware == nil ? .secondary : .primary)
                Spacer()
                Button("Choose…") { choose() }
            }
            TextField("Name", text: $name, prompt: Text(profile.uppercased()))
            if !model.message.isEmpty {
                Text(model.message).font(.callout).foregroundStyle(.red)
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { done() }
                Button(busy ? "Creating…" : "Create") {
                    guard let firmware else { return }
                    busy = true
                    Task {
                        let id = await model.createAvd(profile: profile, firmware: firmware, name: name)
                        busy = false
                        if id != nil { done() }
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(firmware == nil || busy)
            }
        }
        .padding()
        .frame(width: 440)
    }

    private func choose() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.message = "Choose a flash image (.bin) or an ESP-IDF build directory"
        guard panel.runModal() == .OK else { return }
        firmware = panel.url
    }
}
