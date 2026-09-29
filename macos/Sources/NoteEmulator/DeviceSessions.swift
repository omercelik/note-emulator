import AppKit
import SwiftUI

/// One `DeviceModel` per running instance, shared by its device window, its controls window
/// and the Device menu. A model polls while at least one window shows it.
@MainActor
final class DeviceSessions {
    static let shared = DeviceSessions()
    private var models: [String: DeviceModel] = [:]
    private var users: [String: Int] = [:]

    /// The model for a control socket, created and connected on first use.
    func model(for sock: String, name: String = "") -> DeviceModel {
        if let model = models[sock] {
            if model.displayName.isEmpty && !name.isEmpty { model.displayName = name }
            return model
        }
        let model = DeviceModel(socketPath: sock)
        if !name.isEmpty { model.displayName = name }
        models[sock] = model
        model.start()
        return model
    }

    func retain(_ sock: String) {
        users[sock, default: 0] += 1
    }

    /// A window closed. The last one detaches the model (the emulator keeps running).
    func release(_ sock: String) {
        users[sock, default: 1] -= 1
        guard users[sock, default: 0] <= 0, let model = models[sock] else { return }
        users[sock] = nil
        models[sock] = nil
        Task { await model.releaseFocus(); model.detach() }
    }
}

/// Actions shared by the menu, the device window and the controls window.
@MainActor
enum DeviceActions {
    static func screenshot(_ model: DeviceModel) {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.png]
        panel.nameFieldStringValue = "\(model.fileStem)-screenshot.png"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do { try model.saveScreenshot(to: url) } catch { model.lastError = "screenshot: \(error)" }
    }

    static func toggleRecording(_ model: DeviceModel) {
        if model.recording {
            Task { await model.stopRecording() }
            return
        }
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.mpeg4Movie]
        panel.nameFieldStringValue = "\(model.fileStem)-recording.mp4"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        model.startRecording(to: url)
    }

    static func saveSnapshot(_ model: DeviceModel, name: String? = nil) {
        let stamp = Date().formatted(.iso8601.year().month().day().time(includingFractionalSeconds: false))
            .replacingOccurrences(of: ":", with: "")
        let chosen = (name?.isEmpty == false ? name! : "snap-\(stamp)")
        Task { await model.saveSnapshot(chosen) }
    }

    static func exportLog(_ model: DeviceModel) {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.plainText]
        panel.nameFieldStringValue = "\(model.fileStem)-log.txt"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do { try model.exportText().write(to: url, atomically: true, encoding: .utf8) } catch { model.lastError = "export: \(error)" }
    }
}

/// The device the frontmost device window shows (for the Device menu).
struct FocusedDeviceKey: FocusedValueKey {
    typealias Value = DeviceModel
}

extension FocusedValues {
    var device: DeviceModel? {
        get { self[FocusedDeviceKey.self] }
        set { self[FocusedDeviceKey.self] = newValue }
    }
}
