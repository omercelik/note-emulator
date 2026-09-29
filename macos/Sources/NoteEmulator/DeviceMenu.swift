import AppKit
import SwiftUI

/// The Device menu: the menu bar's (for the frontmost device window) and the frame's right
/// click. Keyboard shortcuts are declared once here.
struct DeviceMenuItems: View {
    let model: DeviceModel?
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Button("Controls…") {
            if let model { openWindow(id: "controls", value: model.socketPath) }
        }
        .keyboardShortcut("k", modifiers: [.command, .shift])
        .disabled(model == nil)
        Divider()
        Button("Screenshot…") { if let model { DeviceActions.screenshot(model) } }
            .keyboardShortcut("s")
            .disabled(model?.image == nil)
        Button(model?.recording == true ? "Stop Recording" : "Record Screen…") {
            if let model { DeviceActions.toggleRecording(model) }
        }
        .keyboardShortcut("r")
        .disabled(model?.connected != true)
        Divider()
        Button("Save Snapshot") { if let model { DeviceActions.saveSnapshot(model) } }
            .keyboardShortcut("s", modifiers: [.command, .shift])
            .disabled(model?.connected != true)
        Menu("Load Snapshot") {
            ForEach(model?.snapshots ?? [], id: \.self) { name in
                Button(name) { if let model { Task { await model.loadSnapshot(name) } } }
            }
        }
        .disabled(model?.snapshots.isEmpty ?? true)
        Divider()
        Button("Rotate Left") { model?.rotation = ((model?.rotation ?? 0) + 3) % 4 }
            .keyboardShortcut(.leftArrow, modifiers: .command)
            .disabled(model?.skin == nil)
        Button("Rotate Right") { model?.rotation = ((model?.rotation ?? 0) + 1) % 4 }
            .keyboardShortcut(.rightArrow, modifiers: .command)
            .disabled(model?.skin == nil)
        Button("Zoom to Fit") { model?.zoom = 0 }.keyboardShortcut("0")
        Button("Actual Pixels") { model?.zoom = 1 }.keyboardShortcut("1")
        Button("Double Size") { model?.zoom = 2 }.keyboardShortcut("2")
        Divider()
        Button(model?.speakerMuted == true ? "Unmute Speaker" : "Mute Speaker") {
            model?.speakerMuted.toggle()
        }
        .keyboardShortcut("m", modifiers: [.command, .shift])
        .disabled(model == nil)
        Button(model?.paused == true ? "Resume" : "Pause") {
            if let model { Task { await model.togglePause() } }
        }
        .keyboardShortcut("p", modifiers: [.command, .shift])
        .disabled(model?.connected != true)
        Button("Restart") { if let model { Task { await model.restart() } } }
            .keyboardShortcut("r", modifiers: [.command, .shift])
            .disabled(model?.connected != true)
        Divider()
        // Closing the device window (⌘W) also turns it off. Either way its state is saved for
        // the next Open, and its window closes once the emulator has exited.
        Button("Turn Off") { if let model { Task { await model.stop() } } }
            .disabled(model?.connected != true)
    }
}

struct DeviceCommands: Commands {
    @FocusedValue(\.device) private var model

    var body: some Commands {
        CommandMenu("Device") {
            DeviceMenuItems(model: model)
        }
    }
}
