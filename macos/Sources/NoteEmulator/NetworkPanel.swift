import AppKit
import NoteSession
import SwiftUI

/// Controls ▸ Network, the same for a running and a stopped device. Grouped by what the user
/// wants: internet for the device, opening it from this Mac (and the helper that needs), the
/// device's own setup hotspot, and the addresses that open right now.
struct NetworkPanel: View {
    let avd: String
    /// The running device, or nil when it is stopped.
    var model: DeviceModel?
    @State private var settings = NetworkLaunchSettings.shared
    @State private var helper: NetworkHelperState = .checking
    @State private var installing = false
    @State private var helperError = ""
    @State private var restarting = false
    @Environment(\.openWindow) private var openWindow
    private var manager: ManagerModel { ManagerModel.shared }

    private var record: AvdRecord? { manager.avds.first { $0.id == avd } }
    private var hasHotspot: Bool { record?.profile == "note4c" }
    private var saved: String { settings.mode(for: avd) }
    private var station: Bool { settings.stationAddress(for: avd) }
    /// Either choice that puts an address on this Mac needs the root helper.
    private var needsHelper: Bool { saved == "setup" || station }
    /// This run could not get the helper's addresses and uses a local address instead.
    private var fellBack: Bool { model?.networkPermission.hasPrefix("helper-unavailable") ?? false }
    /// Saved settings the running device has not started with yet.
    private var pending: Bool {
        guard let model else { return false }
        return model.networkActive != saved || model.bindings.contains("10.0.2.15:80") != station
    }

    var body: some View {
        Form {
            internetSection
            if saved != "disabled" {
                accessSection
            }
            if let model, hasHotspot, ["user", "setup"].contains(model.networkActive) {
                HotspotSection(model: model)
            }
            if let model {
                addressesSection(model)
            }
        }
        .formStyle(.grouped)
        .task(id: "\(saved) \(station)") { await refreshHelper() }
    }

    // MARK: - Internet

    private var internetSection: some View {
        Section {
            Toggle(isOn: Binding(
                get: { saved != "disabled" },
                set: { on in choose(on ? "user" : "disabled") }
            )) {
                Text("Internet access")
                Text(saved == "disabled"
                     ? "Off: the device has no network at all."
                     : "The device can join a simulated Wi-Fi network that reaches the internet through this Mac.")
            }
            if saved != "disabled" {
                WifiEnvRow(avd: avd, running: model != nil)
            }
            if !settings.lastError.isEmpty {
                Text(settings.lastError).font(.caption).foregroundStyle(.red)
            }
            pendingRow
        } header: {
            Text("Wi-Fi for the device")
        }
    }

    // MARK: - Opening the device from this Mac

    private var accessSection: some View {
        Section {
            Picker(selection: Binding(get: { saved == "setup" ? "setup" : "user" }, set: { choose($0) })) {
                option("http://127.0.0.1:8080", "A local address. Works right away.").tag("user")
                option("http://192.168.4.1", "The address the device shows on its screen. Needs the network helper.").tag("setup")
            } label: {
                Text(hasHotspot ? "Setup page" : "Web page")
                Text(hasHotspot ? "Where its setup hotspot page opens." : "Where the device's web page opens.")
            }
            .pickerStyle(.radioGroup)
            Toggle(isOn: Binding(get: { station }, set: { on in chooseStation(on) })) {
                Text(verbatim: "Wi-Fi address http://10.0.2.15")
                Text("The address the device shows after it joins Wi-Fi. Needs the network helper.")
            }
            if needsHelper {
                helperRow
            }
            if fellBack {
                Label("The network helper isn't ready, so this run uses http://127.0.0.1:8080. Install it here, then restart the device.",
                      systemImage: "exclamationmark.triangle")
                    .font(.callout).foregroundStyle(.orange)
            }
        } header: {
            Text("Opening the device from this Mac")
        } footer: {
            if saved == "shared" {
                Text("This device was set to Shared networking, which is not available yet. Pick one of the options above.")
                    .foregroundStyle(.orange)
            }
        }
    }

    private func option(_ address: String, _ detail: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(verbatim: address)
            Text(detail).font(.caption).foregroundStyle(.secondary)
        }
    }

    @ViewBuilder private var helperRow: some View {
        LabeledContent {
            switch helper {
            case .checking:
                ProgressView().controlSize(.small)
            case .installed:
                Label("Installed", systemImage: "checkmark.circle.fill").foregroundStyle(.green)
            case .missing, .outdated:
                Button(installing ? "Installing…" : (helper == .missing ? "Install…" : "Update…")) {
                    Task { await installHelper() }
                }
                .disabled(installing)
            }
        } label: {
            Text("Network helper")
            Text(helperExplanation)
        }
        if !helperError.isEmpty {
            Text(helperError).font(.caption).foregroundStyle(.red)
        }
    }

    private var helperExplanation: String {
        switch helper {
        case .checking: return "Checking…"
        case .installed: return "Puts the device's addresses on this Mac. Nothing else to do."
        case .missing: return "Puts the device's addresses on this Mac. Installing asks once for an administrator password."
        case .outdated: return "This version of the app needs a newer helper. Updating asks once for an administrator password."
        }
    }

    // MARK: - Pending changes

    @ViewBuilder private var pendingRow: some View {
        if let model, pending {
            let blocked = needsHelper && helper != .installed
            HStack {
                Label(blocked ? "Install the network helper below, then restart the device."
                              : "Changes apply when the device restarts.",
                      systemImage: "arrow.clockwise.circle")
                    .foregroundStyle(.orange)
                Spacer()
                Button(restarting ? "Restarting…" : "Restart Now") { Task { await restartDevice(model) } }
                    .disabled(restarting || blocked)
            }
        } else if model == nil {
            Text("Changes apply when the device starts.").font(.caption).foregroundStyle(.secondary)
        }
    }

    // MARK: - Addresses

    private func addressesSection(_ model: DeviceModel) -> some View {
        Section("Addresses you can open now") {
            if model.bindings.isEmpty {
                Text(model.networkActive == "disabled"
                     ? "None. Turn on Internet access, then restart the device."
                     : "None yet.")
                    .foregroundStyle(.secondary)
            }
            ForEach(model.bindings, id: \.self) { binding in
                let url = "http://" + (binding.hasSuffix(":80") ? String(binding.dropLast(3)) : binding) + "/"
                LabeledContent {
                    HStack {
                        Button("Copy") {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(url, forType: .string)
                        }
                        Button("Open") { if let link = URL(string: url) { NSWorkspace.shared.open(link) } }
                    }
                } label: {
                    Text(url).textSelection(.enabled)
                    Text(purpose(of: binding))
                }
            }
        }
    }

    private func purpose(of binding: String) -> String {
        if binding.hasPrefix("192.168.4.1:") {
            return hasHotspot ? "The device's setup hotspot page" : "The device's web page"
        }
        if binding.hasPrefix("10.0.2.15:") { return "The device's web page, once it has joined Wi-Fi" }
        return hasHotspot ? "The device's setup hotspot page, through a local port" : "The device's web page, through a local port"
    }

    // MARK: - Actions

    private func choose(_ mode: String) {
        guard saved != mode else { return }
        Task {
            await settings.setMode(mode, for: avd)
            guard settings.lastError.isEmpty else { return }
            if let model { await model.configureNetwork(mode) }
            await refreshHelper()
        }
    }

    private func chooseStation(_ on: Bool) {
        guard station != on else { return }
        Task {
            await settings.setStationAddress(on, for: avd)
            await refreshHelper()
        }
    }

    private func refreshHelper() async {
        guard needsHelper else { return }
        helper = .checking
        helper = await NetworkHelperInstaller.state()
    }

    private func installHelper() async {
        installing = true
        defer { installing = false }
        if let error = await NetworkHelperInstaller.install() {
            helperError = error
        } else {
            helperError = ""
        }
        await refreshHelper()
    }

    /// Turn the device off (its state is saved) and on again with the saved network settings.
    private func restartDevice(_ model: DeviceModel) async {
        restarting = true
        defer { restarting = false }
        let name = record?.name ?? ""
        await manager.stop(avd: avd)
        if let sock = await manager.start(avd: avd) {
            openWindow(id: "device", value: DeviceLink(sock: sock, name: name))
        }
    }
}

/// The device's own Wi-Fi hotspot (NOTE4C setup). Numbered, because the order matters.
private struct HotspotSection: View {
    let model: DeviceModel

    var body: some View {
        Section {
            LabeledContent {
                TextField("Password", text: Binding(
                    get: { model.setupHotspotPassword },
                    set: { model.setupHotspotPassword = $0 }
                ))
                .labelsHidden()
                .textFieldStyle(.roundedBorder)
                .frame(maxWidth: 220)
                .disabled(model.accessPointConnection == .connecting)
                .onSubmit { Task { await model.connectAccessPoint() } }
            } label: {
                Text("1. Hotspot password")
                Text("Shown on the device's screen. Leave empty if it shows none.")
            }
            LabeledContent {
                HStack {
                    if model.accessPointConnection == .connecting { ProgressView().controlSize(.small) }
                    if model.accessPointConnection == .connected {
                        Label("Connected", systemImage: "checkmark.circle.fill").foregroundStyle(.green)
                    }
                    Button(model.accessPointConnection == .connecting ? "Connecting…" : "Connect") {
                        Task { await model.connectAccessPoint() }
                    }
                    .disabled(model.accessPointConnection == .connecting)
                }
            } label: {
                Text("2. Connect")
                Text("Joins the hotspot and checks that its setup page answers.")
            }
            LabeledContent {
                Button("Open Setup Page") {
                    if let url = URL(string: model.browserURL) { NSWorkspace.shared.open(url) }
                }
                .disabled(model.accessPointConnection != .connected || model.browserURL.isEmpty)
            } label: {
                Text("3. Set up the device")
                Text("Opens its setup page in your browser. Use the Wi-Fi name and password above.")
            }
            if case .failed(let message) = model.accessPointConnection {
                Text(message).font(.caption).foregroundStyle(.red)
            }
        } header: {
            Text("The device's setup hotspot")
        } footer: {
            Text("Some firmware, like emini Home, starts its own Wi-Fi hotspot for setup. The password is needed again after the device stops.")
        }
    }
}
