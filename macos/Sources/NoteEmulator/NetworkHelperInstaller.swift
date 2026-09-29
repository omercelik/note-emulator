import Foundation
import Darwin

enum NetworkHelperInstaller {
    static var socketPath: String { "/var/run/note-emulator-\(getuid()).sock" }

    static var isAvailable: Bool {
        FileManager.default.fileExists(atPath: socketPath)
    }

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
        let source = "do shell script \"\(appleScriptCommand)\" with administrator privileges"
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
