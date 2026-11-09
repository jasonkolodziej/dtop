public enum LogStreamKind: UInt8, Sendable, Equatable {
    case stdin = 0
    case stdout = 1
    case stderr = 2
}

public struct LogLine: Sendable, Equatable {
    public var stream: LogStreamKind
    public var text: String
    /// When the daemon wrote the line (seconds since the epoch), for `timestamps=1` streams.
    public var timestamp: Double?

    public init(stream: LogStreamKind, text: String, timestamp: Double? = nil) {
        self.stream = stream
        self.text = text
        self.timestamp = timestamp
    }
}

/// Decodes Docker's log/attach stream. Containers without a TTY multiplex stdout/stderr using
/// 8-byte frame headers (`[stream, 0, 0, 0, size(be32)]`); TTY containers send raw bytes.
/// The decoder is incremental so it works for both buffered and followed logs.
public struct LogStreamDecoder: Sendable {
    public enum Mode: Sendable, Equatable {
        case automatic
        case multiplexed
        case raw
    }

    private var mode: Mode
    private var buffer: [UInt8] = []
    private var partialLines: [LogStreamKind: [UInt8]] = [:]
    /// The stream was requested with `timestamps=1`: each line starts with an RFC 3339 time.
    private let timestamps: Bool

    public init(mode: Mode = .automatic, timestamps: Bool = false) {
        self.mode = mode
        self.timestamps = timestamps
    }

    /// Builds a line, splitting off the `timestamps=1` prefix when there is one.
    private func makeLine(_ bytes: [UInt8], stream: LogStreamKind) -> LogLine {
        if timestamps, let space = bytes.firstIndex(of: 32), space >= 20,
           let time = DockerTimestamp.parse(String(decoding: bytes[..<space], as: UTF8.self)) {
            return LogLine(stream: stream, text: Self.clean(Array(bytes[(space + 1)...])), timestamp: time)
        }
        return LogLine(stream: stream, text: Self.clean(bytes))
    }

    public mutating func feed(_ bytes: [UInt8]) -> [LogLine] {
        buffer.append(contentsOf: bytes)
        if mode == .automatic {
            guard let detected = Self.detectMode(buffer) else { return [] }
            mode = detected
        }
        var lines: [LogLine] = []
        switch mode {
        case .raw, .automatic:
            appendText(buffer, stream: .stdout, into: &lines)
            buffer.removeAll()
        case .multiplexed:
            while buffer.count >= 8 {
                let size = Int(buffer[4]) << 24 | Int(buffer[5]) << 16 | Int(buffer[6]) << 8 | Int(buffer[7])
                guard buffer.count >= 8 + size else { break }
                let stream = LogStreamKind(rawValue: buffer[0]) ?? .stdout
                appendText(Array(buffer[8..<(8 + size)]), stream: stream, into: &lines)
                buffer.removeSubrange(..<(8 + size))
            }
        }
        return lines
    }

    /// Emits any trailing text that did not end with a newline.
    public mutating func finish() -> [LogLine] {
        if mode == .automatic, !buffer.isEmpty {
            mode = .raw
            return feed([]) + finish()
        }
        var lines: [LogLine] = []
        for stream in [LogStreamKind.stdin, .stdout, .stderr] {
            if let partial = partialLines[stream], !partial.isEmpty {
                lines.append(makeLine(partial, stream: stream))
            }
        }
        partialLines.removeAll()
        return lines
    }

    /// A multiplexed stream starts with a stream byte of 0-2 followed by three zero bytes.
    static func detectMode(_ bytes: [UInt8]) -> Mode? {
        guard let first = bytes.first else { return nil }
        guard first <= 2 else { return .raw }
        if bytes.count < 4 {
            // Could still be a header; wait for more bytes unless something already rules it out.
            return bytes.dropFirst().allSatisfy { $0 == 0 } ? nil : .raw
        }
        return bytes[1] == 0 && bytes[2] == 0 && bytes[3] == 0 ? .multiplexed : .raw
    }

    private mutating func appendText(_ bytes: [UInt8], stream: LogStreamKind, into lines: inout [LogLine]) {
        var pending = partialLines[stream] ?? []
        for byte in bytes {
            if byte == 10 {
                lines.append(makeLine(pending, stream: stream))
                pending.removeAll(keepingCapacity: true)
            } else {
                pending.append(byte)
            }
        }
        partialLines[stream] = pending
    }

    /// Strips carriage returns and terminal escape sequences that would corrupt our own drawing.
    static func clean(_ bytes: [UInt8]) -> String {
        var output: [UInt8] = []
        output.reserveCapacity(bytes.count)
        var index = 0
        while index < bytes.count {
            let byte = bytes[index]
            if byte == 0x1B {
                index += 1
                if index < bytes.count, bytes[index] == UInt8(ascii: "[") {
                    index += 1
                    while index < bytes.count, !(0x40...0x7E).contains(bytes[index]) { index += 1 }
                } else if index < bytes.count, bytes[index] == UInt8(ascii: "]") {
                    // OSC: terminated by BEL or ESC \
                    while index < bytes.count, bytes[index] != 0x07 {
                        if bytes[index] == 0x1B, index + 1 < bytes.count, bytes[index + 1] == UInt8(ascii: "\\") {
                            index += 1
                            break
                        }
                        index += 1
                    }
                }
                index += 1
                continue
            }
            if byte == 9 {
                output.append(contentsOf: [32, 32, 32, 32])
            } else if byte >= 0x20, byte != 0x7F {
                output.append(byte)
            }
            index += 1
        }
        return String(decoding: output, as: UTF8.self)
    }
}
