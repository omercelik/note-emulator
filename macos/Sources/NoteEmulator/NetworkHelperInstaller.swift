import Foundation
import Darwin

/// Whether the root helper that puts a device's own addresses on this Mac is ready.
enum NetworkHelperState: Equatable {
    case checking, installed, missing, outdated
}

enum NetworkHelperInstaller {
    static var socketPath: String { "/var/run/note-emulator-\(getuid()).sock" }

    static var isAvailable: Bool {
        FileManager.default.fileExists(atPath: socketPath)
    }

    /// False when the installed helper speaks an older lease protocol than this app's, so an
    /// update reinstalls it (an administrator prompt) only when the helper really changed.
    /// The bundled helper asks the running one (`note-net-helper check`); outside a bundle
    /// there is nothing to compare, and the installed helper is used as it is.
    static var isCurrent: Bool {
        guard let bundled = Bundle.main.executableURL?.deletingLastPathComponent()
            .appendingPathComponent("note-net-helper"),
              FileManager.default.isExecutableFile(atPath: bundled.path) else { return true }
        let process = Process()
        process.executableURL = bundled
        process.arguments = ["check", "--socket", socketPath]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        do { try process.run() } catch { return true }
        process.waitUntilExit()
        // 3: older protocol. 0: current. 4 (nothing answers) is left to the launch retry.
        return process.terminationStatus != 3
    }

    /// The helper's state, checked off the main thread (it asks the running helper).
    static func state() async -> NetworkHelperState {
        guard isAvailable else { return .missing }
        return await Task.detached { isCurrent ? NetworkHelperState.installed : .outdated }.value
    }

    /// Shown in macOS's administrator window, so the password request explains itself.
    static let prompt = "NOTE Emulator wants to install its network helper. It lets the addresses a device shows on its screen, such as 192.168.4.1 and 10.0.2.15, open in your browser on this Mac."

    /// macOS presents its standard administrator authentication window. The app never reads a
    /// password; only the bundled installer and the numeric UID are passed to the privileged shell.
    static func install() async -> String? {
        guard let script = Bundle.main.resourceURL?.appendingPathComponent("install-helper.sh"),
              FileManager.default.isReadableFile(atPath: script.path) else {
            return "The bundled network helper installer is missing. Rebuild the app bundle."
        }
        let uid = getuid()
        let quotedPath = "'" + script.path.replacingOccurrences(of: "'", with: "'\\''") + "'"
        let command = "/bin/sh \(quotedPath) install-root \(uid)"
        let appleScriptCommand = command.replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        let source = "do shell script \"\(appleScriptCommand)\" with prompt \"\(prompt)\" with administrator privileges"
        return await Task.detached {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/osascript")
            process.arguments = ["-e", source]
            let output = Pipe()
            process.standardOutput = output
            process.standardError = output
            do { try process.run() } catch { return error.localizedDescription }
            let data = output.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            guard process.terminationStatus == 0 else {
                let message = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
                return message.contains("(-128)") ? "Administrator access was cancelled." :
                    (message.isEmpty ? "The network helper could not be installed." : message)
            }
            return nil
        }.value
    }
}
