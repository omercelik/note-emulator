import Foundation
import NoteSession
import Observation

/// The network mode each AVD starts with. It lives in the AVD's `config.json` (`ndb avd network`),
/// so `note-emu --avd` and `ndb` see the same choice as the app.
@MainActor
@Observable
final class NetworkLaunchSettings {
    static let shared = NetworkLaunchSettings()
    static let modes: [(tag: String, label: String)] = [
        ("disabled", "Off"), ("user", "User (forwards)"), ("setup", "Setup address"), ("shared", "Shared"),
    ]
    private var modes: [String: String] = [:]
    private var station: [String: Bool] = [:]
    var lastError = ""

    func mode(for avd: String) -> String {
        if let mode = modes[avd] { return mode }
        let mode = DataHome.listAvds(DataHome.url()).first { $0.id == avd }?.network ?? "disabled"
        modes[avd] = mode
        return mode
    }

    /// Saves through `ndb avd network`; the picker shows the new mode at once and reverts on failure.
    func setMode(_ next: String, for avd: String) async {
        let previous = mode(for: avd)
        modes[avd] = next
        station[avd] = nil
        let result = await Ndb.run(["avd", "network", avd, next])
        switch result {
        case .success: lastError = ""
        case .failure(let error):
            modes[avd] = previous
            lastError = error.message
        }
    }

    /// Whether `http://10.0.2.15/` opens on this Mac when the device starts.
    func stationAddress(for avd: String) -> Bool {
        if let on = station[avd] { return on }
        guard var record = DataHome.listAvds(DataHome.url()).first(where: { $0.id == avd }) else { return false }
        record.network = mode(for: avd)
        return record.wantsStationAddress
    }

    /// Pins it on or off through `ndb avd station-address`.
    func setStationAddress(_ on: Bool, for avd: String) async {
        let previous = stationAddress(for: avd)
        station[avd] = on
        switch await Ndb.run(["avd", "station-address", avd, on ? "on" : "off"]) {
        case .success: lastError = ""
        case .failure(let error):
            station[avd] = previous
            lastError = error.message
        }
    }

    /// Drop cached modes (the config may have changed outside the app).
    func invalidate() { modes.removeAll(); station.removeAll() }
}

struct NdbError: Error { let message: String }

enum Ndb {
    /// Runs the bundled (or dev) ndb off the main actor. Failure carries its stderr.
    static func run(_ arguments: [String]) async -> Result<String, NdbError> {
        guard let tool = DataHome.ndbBinary(cwd: URL(fileURLWithPath: FileManager.default.currentDirectoryPath)) else {
            return .failure(NdbError(message: "ndb was not found. Build it, or set NDB_BIN."))
        }
        return await Task.detached { () -> Result<String, NdbError> in
            let process = Process()
            process.executableURL = tool
            process.arguments = arguments
            let out = Pipe(), err = Pipe()
            process.standardOutput = out
            process.standardError = err
            do { try process.run() } catch { return .failure(NdbError(message: error.localizedDescription)) }
            let stdout = out.fileHandleForReading.readDataToEndOfFile()
            let stderr = err.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            guard process.terminationStatus == 0 else {
                return .failure(NdbError(message: String(decoding: stderr, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)))
            }
            return .success(String(decoding: stdout, as: UTF8.self))
        }.value
    }
}
