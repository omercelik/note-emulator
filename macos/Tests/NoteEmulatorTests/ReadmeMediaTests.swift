import AppKit
import ImageIO
import NoteSession
import SwiftUI
import Testing
import UniformTypeIdentifiers
@testable import NoteEmulator

/// README media from the app's own views and live devices (`scripts/readme-media.sh` drives it).
/// `NOTE_MEDIA_MODE`:
/// - `still`: one device (`NOTE_MEDIA_SOCK`) after `NOTE_MEDIA_SETTLE` seconds, as a PNG.
/// - `pair`: two devices side by side (`NOTE_MEDIA_SOCK`, `NOTE_MEDIA_SOCK2`).
/// - `anim`: one device for `NOTE_MEDIA_SECONDS`, as an animated PNG (keeps the transparent
///   corners, so it reads on light and dark pages); unchanged frames are merged.
/// - `manager` / `controls`: the manager window, or the Controls window of `NOTE_MEDIA_SOCK`.
/// - `network`: Controls ▸ Network of the running `NOTE_MEDIA_SOCK` (AVD `NOTE_MEDIA_AVD`).
/// Output: `NOTE_MEDIA_OUT`. Scale: `NOTE_MEDIA_SCALE` (device points to pixels).
@MainActor
@Suite(.serialized) struct ReadmeMediaTests {
    private var env: [String: String] { ProcessInfo.processInfo.environment }

    @Test func readmeMedia() async throws {
        guard let mode = env["NOTE_MEDIA_MODE"], let out = env["NOTE_MEDIA_OUT"] else { return }
        let url = URL(fileURLWithPath: out)
        // Device views are 460 points wide: stills at 2x, animations at 1.25x (smaller files).
        let scale = CGFloat(Double(env["NOTE_MEDIA_SCALE"] ?? "") ?? (mode == "anim" ? 1.25 : 2))
        switch mode {
        case "still":
            let model = try await attach(env["NOTE_MEDIA_SOCK"])
            try await Task.sleep(for: .seconds(Double(env["NOTE_MEDIA_SETTLE"] ?? "") ?? 1))
            try writePNG(try render(DeviceFrameContent(model: model), scale: scale), to: url)
        case "pair":
            let left = try await attach(env["NOTE_MEDIA_SOCK"])
            let right = try await attach(env["NOTE_MEDIA_SOCK2"])
            try await Task.sleep(for: .seconds(Double(env["NOTE_MEDIA_SETTLE"] ?? "") ?? 1))
            let pair = HStack(alignment: .top, spacing: 40) {
                DeviceFrameContent(model: left)
                DeviceFrameContent(model: right)
            }
            try writePNG(try render(pair, scale: scale), to: url)
        case "anim":
            let model = try await attach(env["NOTE_MEDIA_SOCK"])
            try await recordAnimation(model, seconds: Double(env["NOTE_MEDIA_SECONDS"] ?? "") ?? 10, scale: scale, to: url)
        case "manager":
            // `NOTE_MEDIA_START`: an AVD the app itself starts, so its row reads "Running".
            let manager = ManagerModel.shared
            manager.reload()
            if let avd = env["NOTE_MEDIA_START"] { _ = await manager.start(avd: avd) }
            defer { manager.stopLaunched() }
            try writePNG(try snapshot(ManagerView(model: manager), size: CGSize(width: 620, height: 150)), to: url)
        case "controls":
            let sock = try #require(env["NOTE_MEDIA_SOCK"])
            ManagerModel.shared.reload()
            let view = DeviceControlsView(sock: sock)
            // Let the model connect and poll once so the tab shows live values.
            let height = Double(env["NOTE_MEDIA_HEIGHT"] ?? "") ?? 560
            let host = hostingWindow(view, size: CGSize(width: 620, height: height))
            try await Task.sleep(for: .seconds(3))
            try writePNG(try cache(host), to: url)
        case "network":
            // Controls ▸ Network of a running device (`NOTE_MEDIA_SOCK`, AVD `NOTE_MEDIA_AVD`).
            ManagerModel.shared.reload()
            let model = try await attach(env["NOTE_MEDIA_SOCK"])
            let view = NetworkPanel(avd: try #require(env["NOTE_MEDIA_AVD"]), model: model)
            let host = hostingWindow(view, size: CGSize(width: 620, height: Double(env["NOTE_MEDIA_HEIGHT"] ?? "") ?? 900))
            try await Task.sleep(for: .seconds(3))
            try writePNG(try cache(host), to: url)
        default:
            Issue.record("unknown NOTE_MEDIA_MODE \(mode)")
        }
    }

    // MARK: - Devices

    private func attach(_ sock: String?) async throws -> DeviceModel {
        let model = DeviceModel(socketPath: try #require(sock, "NOTE_MEDIA_SOCK"))
        model.start()
        for _ in 0..<300 {
            if model.image != nil && model.skin != nil { return model }
            try await Task.sleep(for: .milliseconds(100))
        }
        Issue.record("no frame from \(sock ?? "")")
        return model
    }

    private func recordAnimation(_ model: DeviceModel, seconds: Double, scale: CGFloat, to url: URL) async throws {
        let step = 0.1
        var frames: [(CGImage, Double)] = []
        var last: Data?
        let start = Date()
        while Date().timeIntervalSince(start) < seconds {
            let image = try render(DeviceFrameContent(model: model), scale: scale)
            let bytes = image.dataProvider?.data as Data?
            if let bytes, bytes == last, !frames.isEmpty {
                frames[frames.count - 1].1 += step
            } else {
                frames.append((image, step))
                last = bytes
            }
            try await Task.sleep(for: .milliseconds(Int(step * 1000)))
        }
        // Hold the last state a moment before the loop restarts.
        frames[frames.count - 1].1 += 1.5
        let dest = try #require(CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, frames.count, nil))
        CGImageDestinationSetProperties(dest, [kCGImagePropertyPNGDictionary: [kCGImagePropertyAPNGLoopCount: 0]] as CFDictionary)
        for (image, delay) in frames {
            CGImageDestinationAddImage(dest, image, [kCGImagePropertyPNGDictionary: [kCGImagePropertyAPNGDelayTime: delay]] as CFDictionary)
        }
        #expect(CGImageDestinationFinalize(dest))
        print("readme-media: \(frames.count) distinct frames over \(seconds) s -> \(url.path)")
    }

    // MARK: - Rendering

    private func render(_ view: some View, scale: CGFloat) throws -> CGImage {
        let renderer = ImageRenderer(content: view)
        renderer.scale = scale
        return try #require(renderer.cgImage, "render failed")
    }

    private func hostingWindow(_ view: some View, size: CGSize) -> NSHostingView<some View> {
        let host = NSHostingView(rootView: view.frame(width: size.width, height: size.height))
        host.frame = NSRect(origin: .zero, size: size)
        let window = NSWindow(contentRect: host.frame, styleMask: [.titled], backing: .buffered, defer: false)
        window.contentView = host
        return host
    }

    private func snapshot(_ view: some View, size: CGSize) throws -> CGImage {
        let host = hostingWindow(view, size: size)
        RunLoop.main.run(until: Date().addingTimeInterval(0.6))
        return try cache(host)
    }

    private func cache(_ host: NSView) throws -> CGImage {
        host.layoutSubtreeIfNeeded()
        let rep = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: rep)
        return try #require(rep.cgImage)
    }

    private func writePNG(_ image: CGImage, to url: URL) throws {
        let dest = try #require(CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil))
        CGImageDestinationAddImage(dest, image, nil)
        #expect(CGImageDestinationFinalize(dest))
        print("readme-media: \(image.width)x\(image.height) -> \(url.path)")
    }
}
