import Darwin
import Foundation

public struct AvdRecord: Identifiable, Equatable, Sendable {
    public var id: String
    public var name: String
    public var profile: String
    /// The mode the AVD starts with (`config.json` `network`): disabled, user, setup or shared.
    public var network: String = "disabled"
    /// Dotenv file whose WIFI_SSID / WIFI_PASSWORD the virtual AP uses (`config.json` `wifi_env`).
    public var wifiEnv: String? = nil
    /// Whether `http://10.0.2.15/` is pinned on or off (`config.json` `station_address`);
    /// nil follows the mode (on for setup).
    public var stationAddress: Bool? = nil

    /// What note-emu will do at start: never without a network, else the pin or the mode's default.
    public var wantsStationAddress: Bool {
        switch network {
        case "setup": return stationAddress ?? true
        case "user": return stationAddress ?? false
        default: return false
        }
    }
}

public struct InstanceRecord: Identifiable, Equatable, Sendable {
    public var id: String { instance }
    public var instance: String
    public var avd: String
    public var pid: Int32
    public var sock: String
}

public enum DataHome {
    /// `NOTE_EMU_HOME`, otherwise `~/Library/Application Support/NOTE Emulator`.
    public static func url(environment: [String: String] = ProcessInfo.processInfo.environment) -> URL {
        if let override = environment["NOTE_EMU_HOME"], !override.isEmpty {
            return URL(fileURLWithPath: override, isDirectory: true)
        }
        let home = FileManager.default.homeDirectoryForCurrentUser
        return home.appendingPathComponent("Library/Application Support/NOTE Emulator", isDirectory: true)
    }

    public static func listAvds(_ root: URL) -> [AvdRecord] {
        let dir = root.appendingPathComponent("avd", isDirectory: true)
        guard let entries = try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil) else {
            return []
        }
        return entries.compactMap { entry -> AvdRecord? in
            let config = entry.appendingPathComponent("config.json")
            guard let data = try? Data(contentsOf: config),
                  let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let id = object["id"] as? String,
                  let name = object["name"] as? String,
                  let profile = object["profile"] as? String
            else { return nil }
            return AvdRecord(id: id, name: name, profile: profile, network: object["network"] as? String ?? "disabled",
                             wifiEnv: object["wifi_env"] as? String,
                             stationAddress: object["station_address"] as? Bool)
        }
        .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    /// Instances whose pid is still alive. A dead pid is a stale descriptor.
    public static func listInstances(_ root: URL) -> [InstanceRecord] {
        let dir = root.appendingPathComponent("run", isDirectory: true)
        guard let entries = try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil) else {
            return []
        }
        return entries.compactMap { entry -> InstanceRecord? in
            let file = entry.appendingPathComponent("instance.json")
            guard let data = try? Data(contentsOf: file),
                  let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let instance = object["instance"] as? String,
                  let avd = object["avd"] as? String,
                  let sock = object["sock"] as? String,
                  !sock.isEmpty
            else { return nil }
            let pid = (object["pid"] as? Int).map(Int32.init) ?? Int32(object["pid"] as? Double ?? 0)
            guard processAlive(pid) else { return nil }
            return InstanceRecord(instance: instance, avd: avd, pid: pid, sock: sock)
        }
        .sorted { $0.instance < $1.instance }
    }

    /// `kill(pid, 0)`. Pid 0 and 1 are never treated as an emulator.
    public static func processAlive(_ pid: Int32) -> Bool {
        if pid <= 1 { return false }
        if kill(pid, 0) == 0 { return true }
        return errno != ESRCH
    }

    /// `NOTE_EMU_BIN`, the `note-emu` beside the app's executable (`NOTE Emulator.app`), otherwise
    /// `target/release/note-emu` or `target/debug/note-emu` walking up from `cwd`.
    public static func noteEmuBinary(environment: [String: String] = ProcessInfo.processInfo.environment, cwd: URL,
                                     executableDir: URL? = Bundle.main.executableURL?.deletingLastPathComponent()) -> URL? {
        tool("note-emu", override: environment["NOTE_EMU_BIN"], cwd: cwd, executableDir: executableDir)
    }

    /// `NDB_BIN`, else the same places as `note-emu`.
    public static func ndbBinary(environment: [String: String] = ProcessInfo.processInfo.environment, cwd: URL,
                                 executableDir: URL? = Bundle.main.executableURL?.deletingLastPathComponent()) -> URL? {
        tool("ndb", override: environment["NDB_BIN"], cwd: cwd, executableDir: executableDir)
    }

    static func tool(_ name: String, override: String?, cwd: URL, executableDir: URL?) -> URL? {
        let fm = FileManager.default
        if let override, fm.isExecutableFile(atPath: override) {
            return URL(fileURLWithPath: override)
        }
        if let sibling = executableDir?.appendingPathComponent(name), fm.isExecutableFile(atPath: sibling.path) {
            return sibling
        }
        var dir = cwd
        for _ in 0..<4 {
            for build in ["target/release", "target/debug"] {
                let candidate = dir.appendingPathComponent(build).appendingPathComponent(name)
                if fm.isExecutableFile(atPath: candidate.path) { return candidate }
            }
            dir.deleteLastPathComponent()
        }
        return nil
    }

    /// The mask ROM is imported into the data directory (from the copy shipped with the app, ADR-018).
    public static func romInstalled(_ root: URL) -> Bool {
        FileManager.default.fileExists(atPath: root.appendingPathComponent("rom/esp32s3_rev0_rom.elf").path)
    }
}
