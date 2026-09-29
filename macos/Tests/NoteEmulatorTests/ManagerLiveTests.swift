import Foundation
import NoteSession
import Testing
@testable import NoteEmulator

/// The manager's first-run path through the real `ndb` (NOTE_G4_LIVE=1): the ROM banner's
/// import refuses a wrong file and accepts the real ROM, and New AVD creates a device.
/// Part of the serialized live suite: it sets process-wide environment variables.
extension DeviceWindowLiveTests {
    @Test func romImportAndNewAvdThroughNdb() async throws {
        guard ProcessInfo.processInfo.environment["NOTE_G4_LIVE"] == "1" else { return }
        let repo = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let home = FileManager.default.temporaryDirectory.appendingPathComponent("mgr-\(UUID())")
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        setenv("NOTE_EMU_HOME", home.path, 1)
        setenv("NDB_BIN", repo.appendingPathComponent("target/release/ndb").path, 1)
        defer { unsetenv("NOTE_EMU_HOME"); unsetenv("NDB_BIN") }

        let model = ManagerModel()
        model.reload()
        #expect(!model.romInstalled, "a fresh home shows the ROM banner")

        let wrong = home.appendingPathComponent("not-a-rom.elf")
        try Data("nope".utf8).write(to: wrong)
        await model.importRom(wrong)
        #expect(!model.romInstalled)
        #expect(!model.message.isEmpty, "the refusal is shown: \(model.message)")

        let installed = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/NOTE Emulator/rom/esp32s3_rev0_rom.elf")
        await model.importRom(installed)
        #expect(model.romInstalled, "\(model.message)")
        #expect(model.message.isEmpty)

        let id = await model.createAvd(profile: "note4",
                                       firmware: repo.appendingPathComponent("third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin"),
                                       name: "From the sheet")
        #expect(id != nil, "\(model.message)")
        #expect(model.avds.contains { $0.name == "From the sheet" && $0.profile == "note4" })

        // Start as the manager does (quick boot, host entropy), then stop what it launched.
        setenv("NOTE_EMU_BIN", repo.appendingPathComponent("target/release/note-emu").path, 1)
        defer { unsetenv("NOTE_EMU_BIN") }
        let sock = await model.start(avd: try #require(id))
        #expect(sock != nil, "\(model.message)")
        #expect(model.instance(for: id!) != nil)
        model.stopLaunched()
        for _ in 0..<50 where model.instance(for: id!) != nil {
            try await Task.sleep(for: .milliseconds(100))
            model.reload()
        }
        #expect(model.instance(for: id!) == nil, "stopped and unpublished")
        let snaps = home.appendingPathComponent("avd/\(id!).avd/snapshots/quickboot.snap")
        #expect(FileManager.default.fileExists(atPath: snaps.path), "quick boot saved a snapshot at stop")

        // Open again: it resumes, and the app owns it (closing its window turns it off).
        _ = try #require(await model.start(avd: id!), "\(model.message)")
        #expect(model.owns(id!) && model.status(of: id!) == "Running")
        await model.delete(avd: id!)
        #expect(!model.message.isEmpty, "deleting a running device is refused")
        #expect(model.avds.contains { $0.id == id! }, "a refused delete keeps the device")
        await model.stop(avd: id!)
        #expect(model.instance(for: id!) == nil && model.status(of: id!) == "Stopped")
        #expect(FileManager.default.fileExists(atPath: snaps.path), "turning off saves the state")

        // Cold boot starts from reset (the old state is dropped at once) and still saves at stop.
        _ = try #require(await model.start(avd: id!, quickBoot: false), "\(model.message)")
        #expect(!FileManager.default.fileExists(atPath: snaps.path), "a cold boot never resumes the old state")
        await model.stop(avd: id!)
        #expect(FileManager.default.fileExists(atPath: snaps.path), "the cold-booted session is saved")

        // Erase: flash back to the image and no state to resume.
        await model.erase(avd: id!)
        #expect(model.message.isEmpty, "\(model.message)")
        #expect(!FileManager.default.fileExists(atPath: snaps.path), "erase drops the quick-boot state")

        // Delete removes even named snapshots and the device row, but leaves the source alone.
        let named = snaps.deletingLastPathComponent().appendingPathComponent("mine.snap")
        try Data("snapshot".utf8).write(to: named)
        await model.delete(avd: id!)
        #expect(model.message.isEmpty, "\(model.message)")
        #expect(!model.avds.contains { $0.id == id! })
        #expect(!FileManager.default.fileExists(atPath: home.appendingPathComponent("avd/\(id!).avd").path))
        #expect(FileManager.default.fileExists(atPath: repo.appendingPathComponent("third_party/zectrix-note4-epd-demo/zectrix-note4-epd-demo-v1.0.0.bin").path))

        let emini = try #require(await model.createAvd(profile: "note4c",
            firmware: repo.appendingPathComponent("third_party/emini-home/emini-home-0.6.2-note4c-merged.bin"),
            name: "NOTE4C emini Home", network: "setup"), "\(model.message)")
        let eminiRecord = try #require(model.avds.first { $0.id == emini })
        #expect(eminiRecord.profile == "note4c" && eminiRecord.network == "setup")
        #expect(await model.saveWifiCredentials(ssid: "Test \"Wi-Fi\"", password: "test-password", avd: emini))
        let wifiPath = try #require(model.avds.first { $0.id == emini }?.wifiEnv)
        let wifiFile = URL(fileURLWithPath: wifiPath)
        let wifiText = try String(contentsOf: wifiFile, encoding: .utf8)
        #expect(wifiText.contains("WIFI_SSID=\"Test \"Wi-Fi\"\""))
        let attrs = try FileManager.default.attributesOfItem(atPath: wifiPath)
        #expect((attrs[.posixPermissions] as? NSNumber)?.intValue == 0o600)
        #expect(await model.saveWifiCredentials(ssid: "Test", password: "", avd: emini), "\(model.message)")
        #expect(!(await model.saveWifiCredentials(ssid: "Test\nWIFI_PASSWORD=injected", password: "", avd: emini)))
        #expect(try String(contentsOf: wifiFile, encoding: .utf8) == "WIFI_SSID=\"Test\"\nWIFI_PASSWORD=\"\"\n")
        await model.delete(avd: emini)
        #expect(!model.avds.contains { $0.id == emini })
    }
}
