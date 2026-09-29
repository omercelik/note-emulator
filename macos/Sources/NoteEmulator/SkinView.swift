import AppKit
import NoteSession
import SwiftUI

/// The device shell (profile `skin`) with the live panel in its screen window. Pointer presses
/// on the drawn keys go through the same press/release path as the keyboard.
struct SkinView: View {
    let skin: SkinGeometry
    let model: DeviceModel
    @State private var shell: NSImage?
    @State private var pointerButton: String?

    var body: some View {
        GeometryReader { geo in
            // Only the device's bounds are shown; `scale` maps canvas units to points.
            let crop = skin.bounds
            let scale = min(geo.size.width / crop.width, geo.size.height / crop.height)
            let side = skin.canvas * scale
            ZStack(alignment: .topLeading) {
                if let shell {
                    // No drawn shadow: the window hugs the shell and macOS draws the window's
                    // own shadow around it (as for the iOS Simulator's device window).
                    Image(nsImage: shell).resizable().frame(width: side, height: side)
                } else {
                    RoundedRectangle(cornerRadius: side * 0.06).fill(Color(white: 0.97)).frame(width: side, height: side)
                }
                screen(pixelScale: skin.screen.width * scale / Double(max(model.panel.width, 1)))
                    .frame(width: skin.screen.width * scale, height: skin.screen.height * scale)
                    .offset(x: skin.screen.minX * scale, y: skin.screen.minY * scale)
                ForEach(Array(skin.hitAreas.enumerated()), id: \.offset) { _, area in
                    key(area, scale: scale)
                }
                ForEach(skin.leds, id: \.name) { led in
                    let lit = model.leds[led.name] ?? false
                    Circle()
                        .fill(lit ? Color.green : Color(white: 0.75))
                        .shadow(color: lit ? .green : .clear, radius: 4)
                        .frame(width: led.r * 2 * scale, height: led.r * 2 * scale)
                        .offset(x: (led.cx - led.r) * scale, y: (led.cy - led.r) * scale)
                }
            }
            .frame(width: side, height: side, alignment: .topLeading)
            .offset(x: -crop.minX * scale, y: -crop.minY * scale)
            .frame(width: crop.width * scale, height: crop.height * scale, alignment: .topLeading)
            // Holding the device anywhere but a key moves the window (the keys sit on top and
            // take their own presses).
            .contentShape(Rectangle())
            .gesture(WindowDragGesture())
            .allowsWindowActivationEvents(true)
            // Top-aligned: the device sits right under the window's button strip.
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        }
        .aspectRatio(skin.bounds.width / skin.bounds.height, contentMode: .fit)
        .onAppear { shell = NSImage(contentsOf: skin.image) }
    }

    /// Crisp panel pixels at 1× and above; below that, nearest-neighbour would drop whole pixel
    /// columns and garble text, so the image is filtered.
    @ViewBuilder private func screen(pixelScale: Double) -> some View {
        if let image = model.image {
            Image(decorative: image, scale: 1).resizable().interpolation(pixelScale >= 1 ? .none : .high)
        } else {
            Rectangle().fill(Color(red: 0.86, green: 0.87, blue: 0.87))
                .overlay(Text(model.connected ? "Waiting for a frame" : "Connecting").foregroundStyle(.secondary))
        }
    }

    /// A drawn key: its own hit shape, pressed while the pointer is down on it (highlighted).
    @ViewBuilder private func key(_ area: SkinGeometry.HitArea, scale: Double) -> some View {
        let held = model.heldButtons.contains(area.button)
        let press = DragGesture(minimumDistance: 0)
            .onChanged { _ in
                guard pointerButton != area.button else { return }
                pointerButton = area.button
                Task { await model.press(area.button, down: true) }
            }
            .onEnded { _ in
                pointerButton = nil
                Task { await model.press(area.button, down: false) }
            }
        switch area.shape {
        case let .circle(cx, cy, r):
            Circle().fill(Color.accentColor.opacity(held ? 0.3 : 0.001))
                .frame(width: r * 2 * scale, height: r * 2 * scale)
                .contentShape(Circle())
                .gesture(press)
                .offset(x: (cx - r) * scale, y: (cy - r) * scale)
        case let .rect(rect):
            // Side keys are thin: widen the target a little beyond the drawing.
            RoundedRectangle(cornerRadius: 4).fill(Color.accentColor.opacity(held ? 0.4 : 0.001))
                .frame(width: rect.width * scale + 8, height: rect.height * scale)
                .contentShape(Rectangle())
                .gesture(press)
                .offset(x: rect.minX * scale - 4, y: rect.minY * scale)
        }
    }
}
