import Testing
@testable import NoteEmulator

@MainActor
@Suite struct LogFilterTests {
    @Test func levelSourceAndTagFiltersCombine() {
        let model = DeviceModel(socketPath: "")
        model.lines = [
            LogLine(seq: 1, text: "E (1) wifi: failed", level: "E", tag: "wifi", channels: [1]),
            LogLine(seq: 2, text: "W (2) home3: low battery", level: "W", tag: "home3", channels: [0, 1]),
            LogLine(seq: 3, text: "I (3) home3: STATUS", level: "I", tag: "home3", channels: [1]),
            LogLine(seq: 4, text: "rst:0x1 (POWERON)", channels: [0]),
        ]
        #expect(model.visibleLines.count == 4)
        model.minLevel = "W"
        #expect(model.visibleLines.map(\.seq) == [1, 2], "Warning+ keeps E and W, drops untagged")
        model.minLevel = ""
        model.channel = 0
        #expect(model.visibleLines.map(\.seq) == [2, 4])
        model.channel = -1
        model.query = "home3"
        #expect(model.visibleLines.map(\.seq) == [2, 3], "tag search")
        #expect(model.exportText() == "W (2) home3: low battery\nI (3) home3: STATUS\n")
    }
}
