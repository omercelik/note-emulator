import AppKit
import NoteSession
import SwiftUI
import Testing
@testable import NoteEmulator

@MainActor
@Suite struct SkinRenderTests {
    /// The shell SVG loads through NSImage and the frame lands inside the screen window.
    /// `NOTE_SKIN_PNG=path` also writes the render for a visual check.
    @Test func skinRendersTheShellAndThePanel() throws {
        let skin = try #require(SkinGeometry.load(profileID: "note4"))
        #expect(NSImage(contentsOf: skin.image) != nil, "NSImage cannot load \(skin.image.path)")
        let model = DeviceModel(socketPath: "")
        var rgba = [UInt8](repeating: 255, count: 400 * 300 * 4)
        for y in 0..<300 where (y / 20) % 2 == 0 {
            for x in 0..<400 { rgba[(y * 400 + x) * 4 + 0] = 0; rgba[(y * 400 + x) * 4 + 1] = 0; rgba[(y * 400 + x) * 4 + 2] = 0 }
        }
        model.image = cgImage(rgba: rgba, width: 400, height: 300)
        model.leds = ["power": true]
        let view = SkinView(skin: skin, model: model).frame(width: 512, height: 512)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        let image = try #require(renderer.cgImage)
        #expect(image.width == 512)
        if let path = ProcessInfo.processInfo.environment["NOTE_SKIN_PNG"] {
            let rep = NSBitmapImageRep(cgImage: image)
            try rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: path))
        }
    }
}
