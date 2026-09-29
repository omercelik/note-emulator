import AppKit
import NoteSession
import SwiftUI

/// The device's settings and tools, in their own window (Device ▸ Controls…, ⇧⌘K), so the
/// device window stays a bare device.
struct DeviceControlsView: View {
    /// A control socket, or `avd:<id>` for a device that was stopped when the window opened.
    let sock: String
    @State private var model: DeviceModel?
    @State private var retained: String?
    /// The device this window is for. It follows it: live while it runs, the stopped view
    /// when it stops, live again when it starts.
    @State private var avd: String?
    @Environment(\.dismiss) private var dismiss

    init(sock: String) {
        self.sock = sock
        if sock.hasPrefix("avd:") {
            _model = State(initialValue: nil)
            _avd = State(initialValue: String(sock.dropFirst(4)))
        } else {
            _model = State(initialValue: DeviceSessions.shared.model(for: sock))
            _avd = State(initialValue: DataHome.listInstances(DataHome.url()).first { $0.sock == sock }?.avd)
        }
    }

    private var stoppedName: String {
        ManagerModel.shared.avds.first { $0.id == avd }?.name ?? "Device"
    }

    var body: some View {
        Group {
            if let model {
                DeviceControlsContent(model: model, avd: avd)
            } else if let avd {
                StoppedDeviceControls(avd: avd)
            }
        }
        .frame(minWidth: 560, idealWidth: 620, minHeight: 460, idealHeight: 560)
        .navigationTitle("Controls — \(model?.windowTitle ?? stoppedName)")
        .onAppear { if let model { adopt(model.socketPath) } }
        .onDisappear { releaseModel() }
        .task { await follow() }
    }

    /// Once a second: a running device that went away shows the stopped view; a stopped one
    /// that started shows the live controls. A window whose device is unknown closes.
    private func follow() async {
        var missing = 0
        while !Task.isCancelled {
            let instances = DataHome.listInstances(DataHome.url())
            if let current = model {
                let alive = instances.contains { $0.sock == current.socketPath }
                missing = alive ? 0 : missing + 1
                if missing >= 2 {
                    missing = 0
                    releaseModel()
                    model = nil
                    if avd == nil { dismiss(); return }
                }
            } else if let avd, let running = instances.first(where: { $0.avd == avd }) {
                model = DeviceSessions.shared.model(for: running.sock)
                adopt(running.sock)
            } else if avd == nil {
                dismiss()
                return
            }
            try? await Task.sleep(for: .seconds(1))
        }
    }

    private func adopt(_ live: String) {
        guard retained == nil else { return }
        DeviceSessions.shared.retain(live)
        retained = live
    }

    private func releaseModel() {
        if let retained { DeviceSessions.shared.release(retained) }
        retained = nil
    }
}

/// The controls themselves, without the window wiring.
struct DeviceControlsContent: View {
    @State var model: DeviceModel
    /// The AVD, when the caller already knows it; otherwise looked up once from the registry.
    var avd: String? = nil
    @State private var selectedSection = 0

    var body: some View {
        VStack(spacing: 0) {
            summary
            Divider()
            ControlsSectionPicker(selection: $selectedSection)

            Group {
                switch selectedSection {
                case 1: NetworkTab(model: model, avd: avd)
                case 2: AudioTab(model: model)
                case 3: SnapshotsTab(model: model)
                case 4: LogTab(model: model)
                default: PowerTab(model: model)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(12)
        }
    }

    private var summary: some View {
        HStack(spacing: 10) {
            Circle().fill(model.connected ? (model.paused ? Color.orange : Color.green) : Color.gray).frame(width: 9, height: 9)
            VStack(alignment: .leading, spacing: 1) {
                Text(model.windowTitle).font(.headline)
                Text(model.connected
                     ? "\(model.profile.uppercased()) · \(model.engine) · \(String(format: "%.1f", model.virtualSeconds)) s\(model.paused ? " · paused" : "")"
                     : (model.lastError.isEmpty ? "Connecting" : model.lastError))
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer()
            Button(model.paused ? "Resume" : "Pause") { Task { await model.togglePause() } }.disabled(!model.connected)
            Button("Restart") { Task { await model.restart() } }.disabled(!model.connected)
                .help("Reset the chip: the firmware reboots, flash and settings stay")
        }
        .padding(12)
    }
}

private struct PowerTab: View {
    @Bindable var model: DeviceModel

    var body: some View {
        Form {
            Section("Battery") {
                LabeledContent("Terminal voltage") {
                    HStack {
                        Slider(value: Binding(get: { Double(model.batteryMV) }, set: { model.batteryMV = Int($0) }),
                               in: 3000...4300, step: 10,
                               onEditingChanged: { editing in if !editing { Task { await model.applyBattery() } } })
                        Text("\(model.batteryMV) mV").monospacedDigit().frame(width: 70, alignment: .trailing)
                    }
                }
                LabeledContent("Presets") {
                    HStack {
                        ForEach([("Low", 3500), ("Half", 3900), ("Full", 4150)], id: \.1) { preset in
                            Button("\(preset.0) \(preset.1)") { model.batteryMV = preset.1; Task { await model.applyBattery() } }
                        }
                    }
                }
                LabeledContent("Reported to the guest", value: "\(model.reportedMV) mV")
            }
            Section("USB") {
                Toggle("Cable plugged in", isOn: Binding(get: { model.usbCable }, set: { on in Task { await model.setUSB(cable: on) } }))
                Text("External supply and charger: the firmware sees charging.").font(.caption).foregroundStyle(.secondary)
                Toggle("Computer attached (USB host)", isOn: Binding(get: { model.usbHost }, set: { on in Task { await model.setUSB(host: on) } }))
                Text("Off is a board on battery with no computer: no USB console, and firmware may sleep.").font(.caption).foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
    }
}

private struct NetworkTab: View {
    let model: DeviceModel
    @State var avd: String?
    @State private var hotspotPassword = ""
    @State private var passwordApplied = false

    var body: some View {
        Form {
            Section("Mode") {
                LabeledContent("Active", value: model.networkActive)
                if let avd {
                    NetworkModePicker(avd: avd, model: model)
                    WifiEnvRow(avd: avd, running: true)
                    Text("The selected mode is saved with the device and applies when it next starts. macOS asks for administrator access if setup address needs the network helper.")
                        .font(.callout).foregroundStyle(.secondary)
                }
                if model.restartRequired {
                    Text("Restart the device to apply \(model.networkConfigured).").font(.caption).foregroundStyle(.orange)
                }
            }
            Section("Browser access") {
                if model.browserURL.isEmpty {
                    Text("No address from the Mac. Start the device with a forward or the setup address.").foregroundStyle(.secondary)
                } else {
                    LabeledContent("URL") { Text(model.browserURL).textSelection(.enabled) }
                    HStack {
                        Button("Copy") {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(model.browserURL, forType: .string)
                        }
                        Button("Open in Browser") { if let url = URL(string: model.browserURL) { NSWorkspace.shared.open(url) } }
                    }
                }
            }
            if model.networkActive == "user" || model.networkActive == "setup" {
                Section("Device setup hotspot") {
                    Text("If the device shows a Wi-Fi password, enter it here so this Mac can open its setup page. This password applies until the device stops.")
                        .font(.callout).foregroundStyle(.secondary)
                    SecureField("Password shown on the device", text: $hotspotPassword)
                    Button("Apply Password") {
                        Task {
                            passwordApplied = await model.connectSetupHotspot(password: hotspotPassword)
                            if passwordApplied { hotspotPassword = "" }
                        }
                    }
                    .disabled(!(8...63).contains(hotspotPassword.utf8.count))
                    if passwordApplied { Text("Password applied. Open the browser URL above and use the device's pairing code.").font(.caption) }
                }
            }
        }
        .formStyle(.grouped)
        .task {
            // Read the registry once, not on every redraw.
            if avd == nil { avd = DataHome.listInstances(DataHome.url()).first { $0.sock == model.socketPath }?.avd }
        }
    }
}

/// Chooses the mode saved in the AVD's config. With a running `model`, the runtime is told too,
/// so it reports that a restart is needed.
private struct NetworkModePicker: View {
    let avd: String
    var model: DeviceModel? = nil
    @State private var settings = NetworkLaunchSettings.shared
    @State private var showRestartAlert = false

    var body: some View {
        Picker("Network mode at start", selection: Binding(
            get: { settings.mode(for: avd) },
            set: { mode in
                guard settings.mode(for: avd) != mode else { return }
                Task {
                    await settings.setMode(mode, for: avd)
                    guard settings.lastError.isEmpty, let model else { return }
                    await model.configureNetwork(mode)
                    showRestartAlert = true
                }
            }
        )) {
            ForEach(NetworkLaunchSettings.modes, id: \.tag) { Text($0.label).tag($0.tag) }
        }
        .alert("Restart needed", isPresented: $showRestartAlert) {
            Button("OK", role: .cancel) {}
        } message: {
            Text("The network change will take effect after you stop this device and start it again.")
        }
        if !settings.lastError.isEmpty {
            Text(settings.lastError).font(.caption).foregroundStyle(.red)
        }
    }
}

/// The five sections, the same whether the device is running or stopped.
private struct ControlsSectionPicker: View {
    @Binding var selection: Int

    var body: some View {
        Picker("Controls section", selection: $selection) {
            Text("Power").tag(0)
            Text("Network").tag(1)
            Text("Audio & Capture").tag(2)
            Text("Snapshots").tag(3)
            Text("Log").tag(4)
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .padding(.horizontal, 16)
        .padding(.top, 12)
    }
}

/// The virtual AP's Wi-Fi credentials: a dotenv file (mode 0600) with WIFI_SSID / WIFI_PASSWORD,
/// for firmware with compiled-in home Wi-Fi. Only the path is stored.
private struct WifiEnvRow: View {
    let avd: String
    var running = false
    private var manager: ManagerModel { ManagerModel.shared }

    private var path: String? { manager.avds.first { $0.id == avd }?.wifiEnv }

    var body: some View {
        LabeledContent("Wi-Fi credentials") {
            HStack {
                Text(path.map { ($0 as NSString).abbreviatingWithTildeInPath } ?? "None: open network \"esp32sim\"")
                    .foregroundStyle(path == nil ? .secondary : .primary)
                    .lineLimit(1).truncationMode(.middle)
                Button("Choose…") { choose() }
                if path != nil {
                    Button("Clear") { Task { await manager.setWifiEnv(nil, avd: avd) } }
                }
            }
        }
        .help("A .env file (mode 0600) whose WIFI_SSID and WIFI_PASSWORD the virtual access point uses, so firmware with your home Wi-Fi compiled in can join it.\(running ? " Applies at the next start." : "")")
    }

    private func choose() {
        let panel = NSOpenPanel()
        panel.showsHiddenFiles = true
        panel.canChooseDirectories = false
        panel.message = "Choose a .env file with WIFI_SSID and WIFI_PASSWORD (mode 0600)"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task { await manager.setWifiEnv(url, avd: avd) }
    }
}

/// A stopped device: the same header and sections as a running one. What can be set offline is
/// editable; what needs the emulator says so. Once the device starts, the window turns live.
private struct StoppedDeviceControls: View {
    let avd: String
    @State private var selectedSection = 1
    @Environment(\.openWindow) private var openWindow
    private var manager: ManagerModel { ManagerModel.shared }

    private var record: AvdRecord? { manager.avds.first { $0.id == avd } }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Circle().fill(Color.gray).frame(width: 9, height: 9)
                VStack(alignment: .leading, spacing: 1) {
                    Text(record?.name ?? "Device").font(.headline)
                    Text("\((record?.profile ?? "").uppercased()) · stopped").font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Menu(manager.launching == avd ? "Starting…" : "Start") {
                    Button("Cold boot") { start(quickBoot: false) }
                } primaryAction: {
                    start(quickBoot: true)
                }
                .fixedSize()
                .disabled(manager.launching == avd)
            }
            .padding(12)
            Divider()
            ControlsSectionPicker(selection: $selectedSection)
            Group {
                switch selectedSection {
                case 1:
                    Form {
                        Section("Mode") {
                            NetworkModePicker(avd: avd)
                            WifiEnvRow(avd: avd)
                            Text("This mode applies when the device starts. User forwards a loopback port; setup address makes 192.168.4.1 available from this Mac; shared needs the vmnet entitlement. macOS asks for administrator access only if the network helper needs it.")
                                .font(.callout).foregroundStyle(.secondary)
                        }
                    }
                    .formStyle(.grouped)
                case 3: StoppedSnapshots(avd: avd)
                case 2: NeedsRunning(text: "Speaker, microphone, screenshots and recording work while the device runs.")
                case 4: NeedsRunning(text: "The log streams from the running device.")
                default: NeedsRunning(text: "Battery, USB and reset act on the running device.")
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(12)
            if !manager.message.isEmpty {
                Text(manager.message).font(.caption).foregroundStyle(.red).padding(.horizontal, 12).padding(.bottom, 8)
            }
        }
    }

    private func start(quickBoot: Bool) {
        let name = record?.name ?? ""
        Task {
            if let sock = await manager.start(avd: avd, quickBoot: quickBoot) {
                openWindow(id: "device", value: DeviceLink(sock: sock, name: name))
            }
        }
    }
}

private struct NeedsRunning: View {
    let text: String

    var body: some View {
        VStack(spacing: 6) {
            Image(systemName: "power").font(.title2).foregroundStyle(.secondary)
            Text(text).foregroundStyle(.secondary).multilineTextAlignment(.center)
            Text("Start the device to use this.").font(.caption).foregroundStyle(.tertiary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// Snapshots saved in the AVD, read from disk. Saving and loading need the running machine.
private struct StoppedSnapshots: View {
    let avd: String

    private var names: [String] {
        let dir = DataHome.url().appendingPathComponent("avd/\(avd).avd/snapshots")
        let files = (try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? []
        return files.filter { $0.hasSuffix(".snap") }.map { String($0.dropLast(5)) }.sorted()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if names.isEmpty {
                Text("No snapshots saved.").foregroundStyle(.secondary)
            } else {
                List(names, id: \.self) { name in
                    Text(name == "quickboot" ? "quickboot (resumed by Start)" : name)
                }
            }
            Text("Start the device to save, load or delete snapshots. Cold boot ignores the quick-boot snapshot.")
                .font(.caption).foregroundStyle(.secondary)
        }
    }
}

private struct AudioTab: View {
    @Bindable var model: DeviceModel

    var body: some View {
        Form {
            Section("Speaker") {
                Toggle("Mute on this Mac", isOn: $model.speakerMuted)
                LabeledContent("Playing now", value: model.speakerActive ? "yes" : "no")
            }
            Section("Microphone") {
                Toggle("Use this Mac's microphone", isOn: $model.micEnabled)
                if model.micEnabled {
                    LabeledContent("Now", value: model.macMicrophone ? "The device is listening"
                                   : "Turns on when the device listens")
                }
                if model.macMicrophone {
                    ProgressView(value: Double(min(model.micLevel * 3, 1))).help("Input level")
                }
                Text("The device hears this Mac's microphone whenever its firmware listens (for push-to-talk, while you hold the button). macOS asks for permission the first time.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section("Capture") {
                HStack {
                    Button("Screenshot…") { DeviceActions.screenshot(model) }.disabled(model.image == nil)
                    Button(model.recording ? "Stop Recording" : "Record Screen…") { DeviceActions.toggleRecording(model) }
                        .disabled(!model.connected)
                }
                Text("Recordings hold each e-paper state for its virtual duration; a pause adds no length.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section("View") {
                Picker("Zoom", selection: $model.zoom) {
                    Text("Fit").tag(0); Text("1×").tag(1); Text("2×").tag(2)
                }
                .pickerStyle(.segmented)
                Picker("Rotation", selection: $model.rotation) {
                    Text("0°").tag(0); Text("90°").tag(1); Text("180°").tag(2); Text("270°").tag(3)
                }
                .pickerStyle(.segmented)
            }
        }
        .formStyle(.grouped)
    }
}

private struct SnapshotsTab: View {
    let model: DeviceModel
    @State private var name = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                TextField("Name (optional)", text: $name).textFieldStyle(.roundedBorder)
                Button("Save Snapshot") { DeviceActions.saveSnapshot(model, name: name); name = "" }
                    .disabled(!model.connected)
            }
            List {
                ForEach(model.snapshots, id: \.self) { snap in
                    HStack {
                        Image(systemName: "clock.arrow.circlepath").foregroundStyle(.secondary)
                        Text(snap)
                        Spacer()
                        Button("Load") { Task { await model.loadSnapshot(snap) } }
                        Button(role: .destructive) { Task { await model.deleteSnapshot(snap) } } label: { Image(systemName: "trash") }
                            .buttonStyle(.borderless)
                    }
                }
            }
            .overlay { if model.snapshots.isEmpty { Text("No snapshots yet").foregroundStyle(.secondary) } }
            Text(model.snapshotStatus.isEmpty
                 ? "A snapshot is the whole machine. Quick boot saves one automatically when the device stops."
                 : model.snapshotStatus)
                .font(.caption).foregroundStyle(.secondary)
        }
        .padding(8)
    }
}

private struct LogTab: View {
    @Bindable var model: DeviceModel
    @State private var draft = ""
    @State private var ending = "\n"

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                TextField("Filter text or tag", text: $model.query).textFieldStyle(.roundedBorder)
                Picker("Level", selection: $model.minLevel) {
                    Text("All").tag(""); Text("Error").tag("E"); Text("Warning+").tag("W"); Text("Info+").tag("I"); Text("Debug+").tag("D")
                }
                .labelsHidden().frame(width: 110)
                Picker("Source", selection: $model.channel) {
                    Text("Both").tag(-1); Text("UART0").tag(0); Text("USB").tag(1)
                }
                .labelsHidden().frame(width: 90)
            }
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 1) {
                        ForEach(model.visibleLines) { line in
                            Text(line.text)
                                .font(.system(size: 11, design: .monospaced))
                                .foregroundStyle(line.level == "E" ? Color.red : line.level == "W" ? Color.orange : Color.primary)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .textSelection(.enabled)
                                .id(line.id)
                        }
                    }
                }
                .background(Color(nsColor: .textBackgroundColor))
                .onChange(of: model.visibleLines.last?.id) { _, last in if let last { proxy.scrollTo(last, anchor: .bottom) } }
            }
            HStack {
                TextField("Send to the console", text: $draft).textFieldStyle(.roundedBorder).onSubmit(send)
                Picker("Ending", selection: $ending) { Text("LF").tag("\n"); Text("CRLF").tag("\r\n"); Text("None").tag("") }
                    .labelsHidden().frame(width: 80)
                Button("Send", action: send)
                Divider().frame(height: 16)
                Button("Export…") { DeviceActions.exportLog(model) }
                Button("Clear") { Task { await model.clearView() } }
            }
        }
    }

    private func send() {
        let text = draft, lineEnding = ending
        Task { await model.sendConsole(text, ending: lineEnding) }
        draft = ""
    }
}
