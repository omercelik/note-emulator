import AVFoundation

/// Plays the guest speaker (i16 mono PCM from `audio.get`) on the Mac's output. Chunks are
/// queued back to back; a gap in the stream just plays later. The format follows the guest's
/// I2S sample rate and is rebuilt when it changes.
public final class SpeakerPlayer: @unchecked Sendable {
    private let engine = AVAudioEngine()
    private let node = AVAudioPlayerNode()
    private var format: AVAudioFormat?
    public var muted = false {
        didSet { node.volume = muted ? 0 : 1 }
    }
    public private(set) var samplesPlayed: UInt64 = 0

    public init() {
        engine.attach(node)
    }

    /// Queue samples at `rate` Hz. Returns false when the output could not be started.
    @discardableResult
    public func play(_ samples: [Int16], rate: Double) -> Bool {
        guard !samples.isEmpty, rate > 0 else { return true }
        if format?.sampleRate != rate {
            guard let f = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: rate, channels: 1, interleaved: false) else { return false }
            engine.stop()
            engine.disconnectNodeOutput(node)
            engine.connect(node, to: engine.mainMixerNode, format: f)
            format = f
        }
        if !engine.isRunning {
            do { try engine.start() } catch { return false }
            node.play()
        }
        guard let format, let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(samples.count)),
              let channel = buffer.floatChannelData?[0] else { return false }
        buffer.frameLength = AVAudioFrameCount(samples.count)
        for (i, s) in samples.enumerated() { channel[i] = Float(s) / 32768 }
        node.scheduleBuffer(buffer)
        samplesPlayed += UInt64(samples.count)
        return true
    }

    public func stop() {
        node.stop()
        engine.stop()
    }

    /// Little-endian i16 bytes to samples.
    public static func samples(_ bytes: [UInt8]) -> [Int16] {
        stride(from: 0, to: bytes.count - 1, by: 2).map { Int16(bitPattern: UInt16(bytes[$0]) | UInt16(bytes[$0 + 1]) << 8) }
    }
}
