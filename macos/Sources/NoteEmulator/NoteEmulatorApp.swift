import SwiftUI

@main
struct NoteEmulatorApp: App {
    @State private var manager = ManagerModel.shared

    var body: some Scene {
        WindowGroup("NOTE Emulator") {
            ManagerView(model: manager)
        }
        .defaultSize(width: 560, height: 420)
        .commands { DeviceCommands() }

        // The device: frame only, on a clear window sized by the device (like a simulator).
        WindowGroup(id: "device", for: DeviceLink.self) { $link in
            if let link {
                DeviceFrameView(link: link)
                    .containerBackground(.clear, for: .window)
                    .toolbar(removing: .title)
                    .toolbarBackgroundVisibility(.hidden, for: .windowToolbar)
            }
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentSize)
        .defaultSize(width: 460, height: 464)
        .windowBackgroundDragBehavior(.enabled)
        // A device window belongs to one emulator process; after a relaunch that process (and
        // its socket) is usually gone, so these windows are not restored.
        .restorationBehavior(.disabled)

        // Settings and tools for one device, one window per device.
        WindowGroup("Device Controls", id: "controls", for: String.self) { $sock in
            if let sock {
                DeviceControlsView(sock: sock)
            }
        }
        .defaultSize(width: 620, height: 560)
        .restorationBehavior(.disabled)
    }
}
