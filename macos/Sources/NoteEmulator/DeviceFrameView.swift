import AppKit
import NoteSession
import SwiftUI

/// The device window: the device and nothing else, like a simulator. The shell floats on a
/// clear window that is dragged by its bezel; its keys are clicked on the drawn buttons (or
/// ↑ ↓ ⏎). Everything else is in the Device menu and the Controls window (⇧⌘K).
struct DeviceFrameView: View {
    let link: DeviceLink
    @State private var model: DeviceModel
    /// Pointer over the device: the window buttons and the small bar fade in (as in the iOS
    /// Simulator), and fade out when it leaves.
    @State private var hovering = false
    @Environment(\.openWindow) private var openWindow
    @Environment(\.dismiss) private var dismiss

    init(link: DeviceLink) {
        self.link = link
        _model = State(initialValue: DeviceSessions.shared.model(for: link.sock, name: link.name))
    }

    /// Tests and previews: a frame around an existing model.
    init(model: DeviceModel) {
        self.link = DeviceLink(sock: model.socketPath, name: model.displayName)
        _model = State(initialValue: model)
    }

    var body: some View {
        // The window is the device; on hover the window buttons and a small bar appear on its
        // top bezel (as the iOS Simulator shows its controls over the device).
        DeviceFrameContent(model: model)
            .overlay(alignment: .topLeading) {
                if hovering { BezelControls(model: model).transition(.opacity) }
            }
            .overlay(alignment: .bottomTrailing) {
                // Small, in the shell's clear rounded corner, clear of the OK key at every zoom.
                CornerResizeHandle().frame(width: 14, height: 14)
                    .help("Drag to resize the device")
            }
            .ignoresSafeArea()
            .onHover { inside in withAnimation(.easeInOut(duration: 0.15)) { hovering = inside } }
            .background(ClearWindow(aspect: windowAspect))
            .focusable()
            .focusEffectDisabled()
            .focusedSceneValue(\.device, model)
            .onKeyPress(phases: [.down, .up]) { press in
                guard let button = button(for: press.key) else { return .ignored }
                Task { await model.press(button, down: press.phase == .down) }
                return .handled
            }
            .onReceive(NotificationCenter.default.publisher(for: NSWindow.didResignKeyNotification)) { _ in
                Task { await model.releaseFocus() }
            }
            .onAppear { DeviceSessions.shared.retain(link.sock) }
            .onDisappear {
                DeviceSessions.shared.release(link.sock)
                // Closing the window turns the device off (state is saved for the next Open), for
                // devices this app started. One started by ndb or note-emu is only detached.
                let manager = ManagerModel.shared
                if let avd = DataHome.listInstances(DataHome.url()).first(where: { $0.sock == link.sock })?.avd,
                   manager.owns(avd) {
                    Task { await manager.stop(avd: avd) }
                }
            }
            // The emulator stopped (or never came back after a relaunch): close, as a simulator
            // window goes away with its device. Two misses in a row, so a slow start is not one.
            .task {
                var missing = 0
                while !Task.isCancelled {
                    try? await Task.sleep(for: .seconds(1))
                    let alive = DataHome.listInstances(DataHome.url()).contains { $0.sock == link.sock }
                    missing = alive ? 0 : missing + 1
                    if missing >= 2 { dismiss(); return }
                }
            }
            .navigationTitle(model.windowTitle)
            .contextMenu { DeviceMenuItems(model: model) }
    }

    /// Window proportions: exactly the device's bounds. The window keeps them when resized.
    private var windowAspect: CGSize? {
        guard let skin = model.skin else { return nil }
        return CGSize(width: skin.bounds.width, height: skin.bounds.height)
    }

    private func button(for key: KeyEquivalent) -> String? {
        switch key {
        case .upArrow: "up"
        case .downArrow: "down"
        case .return: "ok"
        default: nil
        }
    }
}

/// The plain shell leaves little visible window border. This corner accepts a drag inside the
/// window and resizes it with the shell's aspect ratio, including when macOS does not expose a
/// useful border hit target for a hidden-title-bar window.
private struct CornerResizeHandle: NSViewRepresentable {
    func makeNSView(context: Context) -> Handle { Handle() }
    func updateNSView(_ nsView: Handle, context: Context) {}

    final class Handle: NSView {
        private var startFrame: NSRect?
        private var startPointer: NSPoint?

        override func resetCursorRects() {
            addCursorRect(bounds, cursor: .frameResize(position: .bottomRight, directions: .all))
        }

        override func mouseDown(with event: NSEvent) {
            startFrame = window?.frame
            startPointer = NSEvent.mouseLocation
        }

        override func mouseDragged(with event: NSEvent) {
            guard let window, let startFrame, let startPointer else { return }
            let current = NSEvent.mouseLocation
            let ratio = startFrame.width / startFrame.height
            let horizontal = current.x - startPointer.x
            let vertical = (startPointer.y - current.y) * ratio
            let delta = abs(horizontal) > abs(vertical) ? horizontal : vertical
            let width = min(1600, max(320, startFrame.width + delta))
            let height = width / ratio
            window.setFrame(NSRect(x: startFrame.minX, y: startFrame.maxY - height,
                                   width: width, height: height), display: true)
        }
    }
}

/// The device itself (shell, live panel, status badges), without the window wiring.
struct DeviceFrameContent: View {
    let model: DeviceModel

    var body: some View {
        content.overlay(alignment: .topTrailing) { badges }
    }

    @ViewBuilder private var content: some View {
        if let skin = model.skin {
            SkinView(skin: skin, model: model)
                .rotationEffect(.degrees(Double(model.rotation) * 90))
                .modifier(ZoomFrame(side: fixedSide(skin)))
        } else {
            BareDevice(model: model)
                .frame(minWidth: 320, idealWidth: 460, minHeight: 300, idealHeight: 420)
        }
    }

    /// Recording and a lost connection show as small badges on the frame, not as chrome.
    @ViewBuilder private var badges: some View {
        HStack(spacing: 6) {
            if model.recording {
                Label("REC", systemImage: "record.circle.fill").labelStyle(.iconOnly).foregroundStyle(.red)
                    .help("Recording")
            }
            if model.paused {
                Image(systemName: "pause.circle.fill").foregroundStyle(.orange).help("Paused")
            }
            if !model.connected && !model.lastError.isEmpty {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.yellow).help(model.lastError)
            }
        }
        .font(.title3)
        .padding(18)
    }

    /// Zoom 1× / 2× shows the panel at that many window points per panel pixel; Fit follows
    /// the window size.
    private func fixedSide(_ skin: SkinGeometry) -> CGSize? {
        guard model.zoom > 0, model.panel.width > 0 else { return nil }
        let scale = Double(model.panel.width * model.zoom) / skin.screen.width
        return CGSize(width: skin.bounds.width * scale, height: skin.bounds.height * scale)
    }

}

/// Fit: resizable between 260 and 1600 points, square. Fixed zoom: exactly that size (the
/// window follows, since device windows size to their content).
private struct ZoomFrame: ViewModifier {
    let side: CGSize?
    func body(content: Content) -> some View {
        if let side {
            content.frame(width: side.width, height: side.height)
        } else {
            content.frame(minWidth: 320, idealWidth: 460, maxWidth: 1600, minHeight: 320, idealHeight: 464, maxHeight: 1620)
        }
    }
}

/// A profile without a skin: the panel on a plain rounded body with three keys.
private struct BareDevice: View {
    let model: DeviceModel

    var body: some View {
        VStack(spacing: 14) {
            Group {
                if let image = model.image {
                    Image(decorative: image, scale: 1).resizable().interpolation(.high)
                        .aspectRatio(CGSize(width: max(model.panel.width, 1), height: max(model.panel.height, 1)), contentMode: .fit)
                } else {
                    Rectangle().fill(Color(white: 0.9)).aspectRatio(4.0 / 3.0, contentMode: .fit)
                        .overlay(Text(model.connected ? "Waiting for a frame" : "Connecting").foregroundStyle(.secondary))
                }
            }
            HStack(spacing: 18) {
                key("Up", "up"); key("OK", "ok"); key("Down", "down")
            }
        }
        .padding(22)
        .background(RoundedRectangle(cornerRadius: 28).fill(Color(white: 0.97)).shadow(radius: 6, y: 4))
        .padding(10)
    }

    private func key(_ title: String, _ name: String) -> some View {
        Text(title).frame(width: 70, height: 30)
            .background(model.heldButtons.contains(name) ? Color.accentColor.opacity(0.35) : Color.gray.opacity(0.15), in: Capsule())
            .gesture(DragGesture(minimumDistance: 0)
                .onChanged { _ in Task { await model.press(name, down: true) } }
                .onEnded { _ in Task { await model.press(name, down: false) } })
    }
}

/// Makes the hosting window a see-through carrier for the device: not opaque, no background,
/// its own window buttons hidden (the device draws its own on hover), movable by its content.
private struct ClearWindow: NSViewRepresentable {
    /// Width : height the window keeps while resized (nil: free).
    var aspect: CGSize?

    func makeNSView(context: Context) -> NSView { Carrier() }
    func updateNSView(_ nsView: NSView, context: Context) {
        guard let window = nsView.window else { return }
        // SwiftUI can update the scene's window between representable callbacks. Keep the
        // carrier transparent so the SVG's clear corners show the desktop, not a window fill.
        window.isOpaque = false
        window.backgroundColor = .clear
        window.invalidateShadow() // the shadow follows the shell's shape
        if let aspect, window.contentAspectRatio != aspect {
            window.contentAspectRatio = aspect
            // Bring an existing window (restored at another shape) to these proportions.
            let frame = window.frame
            let height = frame.width * aspect.height / aspect.width
            window.setFrame(NSRect(x: frame.minX, y: frame.maxY - height, width: frame.width, height: height), display: true)
        }
    }

    private final class Carrier: NSView {
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            guard let window else { return }
            window.isOpaque = false
            window.backgroundColor = .clear
            window.hasShadow = true              // macOS shades the shell's outline
            window.styleMask.insert(.fullSizeContentView)
            window.titlebarAppearsTransparent = true
            window.titleVisibility = .hidden
            window.isMovableByWindowBackground = true
            for kind in [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton] {
                window.standardWindowButton(kind)?.isHidden = true
            }
        }
    }
}

/// A Simulator-style window button: a coloured dot that shows its symbol while hovered.
private struct WindowDot: View {
    let color: Color
    let symbol: String
    let size: Double
    let action: () -> Void
    @State private var over = false

    var body: some View {
        Button(action: action) {
            Circle().fill(color)
                .overlay(Circle().strokeBorder(.black.opacity(0.15), lineWidth: 0.5))
                .overlay(Image(systemName: symbol).font(.system(size: size * 0.55, weight: .bold))
                    .foregroundStyle(.black.opacity(over ? 0.55 : 0)))
                .frame(width: size, height: size)
        }
        .buttonStyle(.plain)
        .onHover { over = $0 }
    }
}

/// The hover controls drawn on the device's top bezel.
struct BezelControls: View {
    let model: DeviceModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        if let skin = model.skin {
            GeometryReader { geo in controls(skin: skin, size: geo.size) }
        }
    }

    /// Window buttons at the left of the top bezel, the name and three actions in the middle,
    /// all centred on the bezel between the shell's top edge and the screen.
    private func controls(skin: SkinGeometry, size: CGSize) -> some View {
        let scale = min(size.width / skin.bounds.width, size.height / skin.bounds.height)
        let bodyTop = skin.bounds.minY + 1
        let recessTop = skin.screen.minY - 14
        let centerY = ((bodyTop + recessTop) / 2 - skin.bounds.minY) * scale
        let bezel = (recessTop - bodyTop) * scale
        let control = min(max(bezel * 0.62, 10), 16)
        return ZStack {
            HStack(spacing: control * 0.62) {
                WindowDot(color: Color(red: 1, green: 0.37, blue: 0.34), symbol: "xmark", size: control) {
                    NSApp.keyWindow?.performClose(nil)
                }
                .help("Close window (the device keeps running)")
                WindowDot(color: Color(red: 1, green: 0.74, blue: 0.18), symbol: "minus", size: control) {
                    NSApp.keyWindow?.miniaturize(nil)
                }
                .help("Minimize")
                WindowDot(color: Color(red: 0.16, green: 0.78, blue: 0.25), symbol: "plus", size: control) {
                    model.zoom = (model.zoom + 1) % 3
                }
                .help("Zoom: fit, 1×, 2×")
                Spacer()
            }
            .padding(.leading, 30 * scale + control)
            HStack(spacing: control * 0.8) {
                Text(model.windowTitle).font(.system(size: control * 0.85, weight: .semibold)).lineLimit(1)
                Button { openWindow(id: "controls", value: model.socketPath) } label: { Image(systemName: "slider.horizontal.3") }
                    .help("Controls (⇧⌘K)")
                Button { DeviceActions.screenshot(model) } label: { Image(systemName: "camera") }
                    .help("Screenshot (⌘S)").disabled(model.image == nil)
                Button { model.rotation = (model.rotation + 1) % 4 } label: { Image(systemName: "rotate.right") }
                    .help("Rotate (⌘→)")
            }
            .font(.system(size: control * 0.85))
            .buttonStyle(.borderless)
            .padding(.horizontal, control * 0.8)
            .frame(height: control * 1.5)
            .background(.regularMaterial, in: Capsule())
        }
        .frame(width: size.width)
        .position(x: size.width / 2, y: centerY)
    }

}
