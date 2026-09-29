import CoreGraphics
import Foundation

/// Device shell geometry from a profile's `skin` object (`Profiles/<id>.json`), in the skin's
/// square canvas coordinates.
public struct SkinGeometry: Equatable, Sendable {
    public struct HitArea: Equatable, Sendable {
        public enum Shape: Equatable, Sendable {
            case circle(cx: Double, cy: Double, r: Double)
            case rect(CGRect)
        }
        public var button: String
        public var shape: Shape
    }

    public struct LED: Equatable, Sendable {
        public var name: String
        public var cx: Double
        public var cy: Double
        public var r: Double
    }

    public var canvas: Double
    /// The device's extent on the canvas (shell, side keys, shadow); the whole canvas if absent.
    public var bounds: CGRect
    public var screen: CGRect
    public var hitAreas: [HitArea]
    public var leds: [LED]
    /// The shell drawing, resolved against the profiles directory.
    public var image: URL

    /// The button under `point` (canvas coordinates), if any.
    public func button(at point: CGPoint) -> String? {
        for area in hitAreas {
            switch area.shape {
            case let .circle(cx, cy, r):
                let dx = point.x - cx, dy = point.y - cy
                if dx * dx + dy * dy <= r * r { return area.button }
            case let .rect(rect):
                if rect.contains(point) { return area.button }
            }
        }
        return nil
    }

    public static func decode(profileJSON data: Data, profilesDir: URL) throws -> SkinGeometry {
        guard let root = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let skin = root["skin"] as? [String: Any],
              let screen = skin["screen"] as? [String: Any],
              let imagePath = skin["image"] as? String
        else { throw SkinError.missing("skin") }
        func number(_ object: [String: Any], _ key: String) throws -> Double {
            guard let n = object[key] as? NSNumber else { throw SkinError.missing(key) }
            return n.doubleValue
        }
        var areas: [HitArea] = []
        for area in skin["hit_areas"] as? [[String: Any]] ?? [] {
            guard let button = area["button"] as? String else { throw SkinError.missing("button") }
            switch area["shape"] as? String {
            case "circle":
                areas.append(HitArea(button: button, shape: .circle(cx: try number(area, "cx"), cy: try number(area, "cy"), r: try number(area, "r"))))
            case "rect":
                areas.append(HitArea(button: button, shape: .rect(CGRect(x: try number(area, "x"), y: try number(area, "y"), width: try number(area, "w"), height: try number(area, "h")))))
            default:
                throw SkinError.missing("shape")
            }
        }
        let leds = try (skin["leds"] as? [[String: Any]] ?? []).map { led in
            LED(name: led["led"] as? String ?? "", cx: try number(led, "cx"), cy: try number(led, "cy"), r: try number(led, "r"))
        }
        let canvas = try number(skin, "canvas")
        var bounds = CGRect(x: 0, y: 0, width: canvas, height: canvas)
        if let b = skin["bounds"] as? [String: Any] {
            bounds = CGRect(x: try number(b, "x"), y: try number(b, "y"), width: try number(b, "w"), height: try number(b, "h"))
        }
        return SkinGeometry(
            canvas: canvas,
            bounds: bounds,
            screen: CGRect(x: try number(screen, "x"), y: try number(screen, "y"), width: try number(screen, "w"), height: try number(screen, "h")),
            hitAreas: areas,
            leds: leds,
            image: profilesDir.appendingPathComponent(imagePath)
        )
    }

    /// `NOTE_EMU_PROFILES`, the app bundle's `Profiles`, or a `Profiles` directory above the
    /// working directory or the executable (development checkouts).
    public static func profilesDirectory(environment: [String: String] = ProcessInfo.processInfo.environment) -> URL? {
        let fm = FileManager.default
        if let dir = environment["NOTE_EMU_PROFILES"] { return URL(fileURLWithPath: dir) }
        if let bundled = Bundle.main.resourceURL?.appendingPathComponent("Profiles"),
           fm.fileExists(atPath: bundled.appendingPathComponent("note4.json").path) {
            return bundled
        }
        let starts = [URL(fileURLWithPath: fm.currentDirectoryPath), Bundle.main.executableURL?.deletingLastPathComponent()].compactMap { $0 }
        for start in starts {
            var dir = start
            for _ in 0..<8 {
                let candidate = dir.appendingPathComponent("Profiles")
                if fm.fileExists(atPath: candidate.appendingPathComponent("note4.json").path) { return candidate }
                dir.deleteLastPathComponent()
            }
        }
        return nil
    }

    public static func load(profileID: String) -> SkinGeometry? {
        guard let dir = profilesDirectory(),
              let data = try? Data(contentsOf: dir.appendingPathComponent("\(profileID).json"))
        else { return nil }
        return try? decode(profileJSON: data, profilesDir: dir)
    }
}

public enum SkinError: Error, Equatable {
    case missing(String)
}
