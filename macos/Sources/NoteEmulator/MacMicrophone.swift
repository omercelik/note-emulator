import AVFoundation
import Foundation
import NoteMedia

/// The Mac's microphone, converted to 16 kHz mono i16 and buffered until `take()`. The runtime
/// resamples to the guest's I2S rate and drops audio while the guest is not listening.
protocol MicrophoneCapture: AnyObject {
    var level: Float { get }
    func start() throws
    func stop()
    func take() -> Data
}

final class MacMicrophone: MicrophoneCapture, @unchecked Sendable {
    static let rate = 16_000
    private let engine = AVAudioEngine()
    private let target = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: Double(MacMicrophone.rate),
                                       channels: 1, interleaved: true)!
    private let lock = NSLock()
    private var pending = Data()
    private var converter: AVAudioConverter?
    private var tapInstalled = false
    /// Peak of the last converted buffer (0...1), for a level meter.
    private(set) var level: Float = 0

    /// Asks once; macOS remembers the answer (System Settings ▸ Privacy ▸ Microphone).
    static func requestAccess() async -> Bool {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: return true
        case .notDetermined: return await AVCaptureDevice.requestAccess(for: .audio)
        default: return false
        }
    }

    func start() throws {
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, let converter = AVAudioConverter(from: format, to: target) else {
            throw RecorderError.failed("no microphone input")
        }
        self.converter = converter
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { [weak self] buffer, _ in
            self?.convert(buffer)
        }
        tapInstalled = true
        engine.prepare()
        try engine.start()
    }

    func stop() {
        if tapInstalled {
            engine.inputNode.removeTap(onBus: 0)
            tapInstalled = false
        }
        engine.stop()
        lock.withLock { pending.removeAll() }
        level = 0
    }

    /// Everything captured since the last call.
    func take() -> Data {
        lock.withLock {
            let out = pending
            pending.removeAll(keepingCapacity: true)
            return out
        }
    }

    private func convert(_ buffer: AVAudioPCMBuffer) {
        guard let converter else { return }
        let capacity = AVAudioFrameCount(Double(buffer.frameLength) * target.sampleRate / buffer.format.sampleRate) + 64
        guard let out = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else { return }
        var fed = false
        var error: NSError?
        converter.convert(to: out, error: &error) { _, status in
            if fed {
                status.pointee = .noDataNow
                return nil
            }
            fed = true
            status.pointee = .haveData
            return buffer
        }
        guard error == nil, out.frameLength > 0, let channel = out.int16ChannelData else { return }
        let count = Int(out.frameLength)
        var peak: Int32 = 0
        for i in 0..<count { peak = max(peak, abs(Int32(channel[0][i]))) }
        level = Float(peak) / 32768
        let data = Data(bytes: channel[0], count: count * 2)
        lock.withLock {
            pending.append(data)
            // Never hold more than two seconds if nobody takes it.
            let cap = MacMicrophone.rate * 2 * 2
            if pending.count > cap { pending.removeFirst(pending.count - cap) }
        }
    }
}
